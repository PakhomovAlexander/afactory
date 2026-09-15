use super::*;
use review_core::Producer;
use review_core::task::plan::PlanDependencyV1;
mod broker;
mod lease;
mod owned;
mod planning;
mod recording;
mod report;
mod reservation;
mod retry;
mod review;
mod review_handoff;
mod review_integration;
mod review_round_publication;
mod source;
mod token_scopes;
mod wide_usage;

struct Authority {
    generated: Vec<GeneratedOriginV1>,
    authorization_id: String,
    developer_allowed: bool,
    current: bool,
    valid_until: u64,
    output_allowed: bool,
    retry_allowed: bool,
    corrupt_during_output: Option<std::path::PathBuf>,
}

impl TaskAuthority for Authority {
    fn validate_retry(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &review_core::task::execution::TaskInvocationV1,
        previous: &BTreeMap<String, review_core::task::execution::TaskAttemptResultV1>,
    ) -> Result<(), String> {
        for result in previous.values() {
            match result {
                review_core::task::execution::TaskAttemptResultV1::Failed {
                    diagnostic_id, ..
                }
                | review_core::task::execution::TaskAttemptResultV1::Abandoned { diagnostic_id } => {
                    cas.verify(diagnostic_id).map_err(|e| e.to_string())?;
                }
                _ => return Err("Selected output reached retry policy".into()),
            }
        }
        if self.retry_allowed {
            Ok(())
        } else {
            Err("Captured fixture policy refuses retry".into())
        }
    }
    fn validate_planning_inputs(
        &self,
        _: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        _: &ExecutionPlanV1,
    ) -> Result<(), String> {
        if previous.inputs != next.inputs {
            return Err("Fixture has no root input constructors".into());
        }
        Ok(())
    }
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &[String],
        _: &str,
    ) -> Result<(), String> {
        Ok(())
    }
    fn validate_plan(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        Ok(self.generated.clone())
    }
    fn authorize_decision(
        &self,
        _: &TaskRevisionV1,
        _: &str,
        _: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        if !self.developer_allowed {
            return Err("Worker has no host developer session".into());
        }
        Ok(DeveloperGrant {
            developer: "alice".into(),
            authorization_id: self.authorization_id.clone(),
            valid_until_unix_ms: self.valid_until,
        })
    }
    fn authorization_current(&self, _: &PlanDecisionV1) -> Result<(), String> {
        if self.current {
            Ok(())
        } else {
            Err("Host authorization revoked".into())
        }
    }
    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("No execution receipts exist in this lifecycle fixture".into())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_core::task::execution::TaskOutputV1,
    ) -> Result<(), String> {
        if let Some(path) = &self.corrupt_during_output {
            std::fs::write(path, b"changed during domain validation").map_err(|e| e.to_string())?;
        }
        if self.output_allowed {
            Ok(())
        } else {
            Err("Output schema admission failed".into())
        }
    }
}

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "task-lifecycle-test".into(),
        node_id: None,
        operation_id: "capture".into(),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    cas: Cas,
    store: EventStore,
    revision: TaskRevisionV1,
    revision_id: String,
    plan: ExecutionPlanV1,
    plan_id: String,
    authority: Authority,
}

impl Fixture {
    fn new(generated: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.sqlite");
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .join("fixtures/task-contracts/v1");
        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(root.join(name)).unwrap()).unwrap()
        };
        let policy = cas.put_json(&json!({"trusted_test_policy":1})).unwrap();
        let source = cas
            .put_artifact(
                "af/Requirements@1",
                producer(),
                vec![],
                None,
                json!({"text":"Write a migration guide"}),
            )
            .unwrap()
            .0;
        let mut revision: TaskRevisionV1 =
            serde_json::from_value(read("task-revision.json")).unwrap();
        revision.task_id = "task-1".into();
        revision
            .inputs
            .get_mut("requirements")
            .unwrap()
            .artifact_ids = vec![source.clone()];
        revision.authority.policy_id = policy.clone();
        revision
            .acceptance
            .get_mut("checked")
            .unwrap()
            .verifier_policy = policy.clone();
        revision.provenance.adapter_id = policy.clone();
        revision.provenance.input_artifact_ids = vec![source];
        revision.limits.deadline_unix_ms = now().unwrap() + 1_000_000;
        let revision_id = cas
            .put_artifact(
                task::TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(&revision).unwrap(),
            )
            .unwrap()
            .0;
        let (pipeline_id, pipeline) = cas
            .put_artifact(
                task::PIPELINE_V1,
                producer(),
                vec![],
                None,
                read("pipeline-definition.json"),
            )
            .unwrap();
        let mut plan: ExecutionPlanV1 =
            serde_json::from_value(read("execution-plan.json")).unwrap();
        plan.task_revision_id = revision_id.clone();
        plan.authority = revision.authority.clone();
        plan.limits = revision.limits.clone();
        plan.inputs = revision.inputs.clone();
        plan.engine_id = policy.clone();
        plan.compiled_graph_id = policy.clone(); // The trusted test guard stands in for P03 compilation.
        plan.pipeline_id = pipeline_id.clone();
        plan.bindings.clear();
        plan.dependencies = BTreeMap::from([(
            "builtin/document".into(),
            PlanDependencyV1 {
                name: "builtin/document".into(),
                content_digest: pipeline.content_id,
                artifact_id: pipeline_id.clone(),
            },
        )]);
        plan.generated_origins = if generated {
            vec![GeneratedOriginV1 {
                pipeline_id,
                proposal_id: policy.clone(),
                bootstrap_plan_id: policy.clone(),
            }]
        } else {
            vec![]
        };
        let plan_id = cas
            .put_artifact(
                task::EXECUTION_PLAN_V1,
                producer(),
                vec![revision_id.clone()],
                None,
                serde_json::to_value(&plan).unwrap(),
            )
            .unwrap()
            .0;
        let authority = Authority {
            generated: plan.generated_origins.clone(),
            authorization_id: policy,
            developer_allowed: true,
            current: true,
            valid_until: now().unwrap() + 500_000,
            output_allowed: true,
            retry_allowed: true,
            corrupt_during_output: None,
        };
        Self {
            _dir: dir,
            path: path.clone(),
            cas,
            store: EventStore::open(path).unwrap(),
            revision,
            revision_id,
            plan,
            plan_id,
            authority,
        }
    }

    fn open(&mut self) -> TaskLease {
        self.store
            .open_task(&self.cas, &self.revision_id, "writer-1", 1_000_000)
            .unwrap()
    }
    fn propose(&mut self, lease: &TaskLease) {
        self.store
            .propose_task_plan(&self.cas, lease, &self.plan_id, &self.authority)
            .unwrap();
    }
    fn decide(&mut self, lease: &TaskLease, decision: PlanDecisionKindV1) -> RunEvent {
        self.store
            .decide_task_plan(
                &self.cas,
                lease,
                &self.plan_id,
                decision,
                "Inspected exact plan",
                &self.authority,
            )
            .unwrap()
    }
    fn state(&self) -> TaskProjection {
        self.store
            .task_projection(&self.cas, "task-1")
            .unwrap()
            .unwrap()
    }

    fn with_execution_graph(self) -> Self {
        self.with_execution_graph_outputs(BTreeMap::new())
    }

    fn with_execution_graph_outputs(
        mut self,
        additional: BTreeMap<String, task::pipeline::PipelinePortV1>,
    ) -> Self {
        use review_core::task::pipeline::*;
        use review_graph::task::{
            CompileContext, OperatorAttemptCost, OperatorSignature, compile_task,
        };
        let pipeline: PipelineDefinitionV1 =
            payload(&self.cas, &self.plan.pipeline_id, task::PIPELINE_V1).unwrap();
        let mut output = pipeline.contract.outputs["document"].clone();
        output.covers.clear();
        let signature = OperatorSignature {
            contract: PipelineContractV1 {
                inputs: BTreeMap::from([(
                    "input".into(),
                    pipeline.contract.inputs["requirements"].clone(),
                )]),
                outputs: BTreeMap::from([("output".into(), output)])
                    .into_iter()
                    .chain(additional)
                    .collect(),
            },
            effects: BTreeSet::new(),
            evidence: BTreeMap::from([(
                "output".into(),
                BTreeSet::from([self.revision.acceptance["checked"].verifier_policy.clone()]),
            )]),
            retains: BTreeMap::new(),
            roles: BTreeSet::from(["author".into()]),
            worker_input_type: Some("af/Requirements@1".into()),
            worker_output_type: Some(pipeline.contract.outputs["document"].artifact_type.clone()),
            outcome_port: None,
            attempt: Some(OperatorAttemptCost {
                tokens: 10,
                wall_ms: 1000,
            }),
        };
        let pipelines = BTreeMap::from([(pipeline.name.clone(), pipeline)]);
        let signatures = BTreeMap::from([("worker/builtin/document-author".into(), signature)]);
        let graph = compile_task(
            &self.revision,
            "builtin/document",
            &CompileContext {
                slot_workers: BTreeMap::new(),
                pipelines: &pipelines,
                signatures: &signatures,
                acceptance_outputs: BTreeMap::from([("checked".into(), "document".into())]),
                max_nodes: 64,
                max_depth: 4,
            },
        )
        .unwrap();
        self.plan.compiled_graph_id = self
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
        self.plan_id = self
            .cas
            .put_artifact(
                task::EXECUTION_PLAN_V1,
                producer(),
                vec![self.revision_id.clone()],
                None,
                serde_json::to_value(&self.plan).unwrap(),
            )
            .unwrap()
            .0;
        self
    }

    fn record_execution_inputs(&mut self, lease: &TaskLease) -> String {
        use review_core::task::execution::*;
        let root = self
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                producer(),
                vec![self.plan_id.clone()],
                None,
                serde_json::to_value(TaskInvocationV1 {
                    plan_id: self.plan_id.clone(),
                    node: "root.inputs".into(),
                    inputs: BTreeMap::new(),
                })
                .unwrap(),
            )
            .unwrap()
            .0;
        self.store
            .record_task_invocation(&self.cas, lease, &root, &self.authority)
            .unwrap();
        let root_output = self
            .cas
            .put_artifact(
                TASK_OUTPUT_V1,
                producer(),
                vec![root.clone()],
                None,
                serde_json::to_value(TaskOutputV1 {
                    invocation_id: root,
                    outputs: self.revision.inputs.clone(),
                })
                .unwrap(),
            )
            .unwrap()
            .0;
        self.store
            .publish_task_output(&self.cas, lease, &root_output, None, &self.authority)
            .unwrap();
        let writer = self
            .cas
            .put_artifact(
                TASK_INVOCATION_V1,
                producer(),
                vec![self.plan_id.clone()],
                None,
                serde_json::to_value(TaskInvocationV1 {
                    plan_id: self.plan_id.clone(),
                    node: "root.nodes.write".into(),
                    inputs: BTreeMap::from([(
                        "input".into(),
                        self.revision.inputs["requirements"].clone(),
                    )]),
                })
                .unwrap(),
            )
            .unwrap()
            .0;
        self.store
            .record_task_invocation(&self.cas, lease, &writer, &self.authority)
            .unwrap();
        writer
    }

    fn execution_output(&self, invocation_id: &str, attempt_id: &str) -> String {
        use review_core::task::execution::*;
        let producer = Producer::Attempt {
            run_id: task_run_id(&self.revision.task_id).unwrap(),
            node_id: "root.nodes.write".into(),
            attempt_id: attempt_id.into(),
        };
        let document = self
            .cas
            .put_artifact(
                "af/CheckedDocument@1",
                producer.clone(),
                vec![invocation_id.into()],
                None,
                json!({"outcome":"passed"}),
            )
            .unwrap()
            .0;
        self.cas
            .put_artifact(
                TASK_OUTPUT_V1,
                producer,
                vec![invocation_id.into(), document.clone()],
                None,
                serde_json::to_value(TaskOutputV1 {
                    invocation_id: invocation_id.into(),
                    outputs: BTreeMap::from([(
                        "output".into(),
                        task::ArtifactInputV1 {
                            artifact_ids: vec![document],
                            artifact_type: "af/CheckedDocument@1".into(),
                            cardinality: review_core::PortCardinality::One,
                            snapshot_id: None,
                        },
                    )]),
                })
                .unwrap(),
            )
            .unwrap()
            .0
    }
}

#[test]
fn generated_plan_and_approval_survive_reopen_without_admitting_execution() {
    let mut f = Fixture::new(true);
    let lease = f.open();
    f.propose(&lease);
    assert!(matches!(
        f.state().phase,
        TaskPhaseV1::Waiting {
            reason: TaskWaitingReasonV1::NeedsPlanReview
        }
    ));
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
    f.store = EventStore::open(&f.path).unwrap();
    let decision = f.decide(&lease, PlanDecisionKindV1::Approved);
    assert!(!f.state().admitted, "Approval is not dispatch or admission");
    assert_eq!(
        f.decide(&lease, PlanDecisionKindV1::Approved).event_id,
        decision.event_id
    );
    assert_eq!(
        f.state().next_sequence,
        3,
        "Duplicate approval appends no event"
    );
    f.store = EventStore::open(&f.path).unwrap();
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    assert!(f.state().admitted);
    assert_eq!(f.state().plan_id.as_deref(), Some(f.plan_id.as_str()));
}

#[test]
fn worker_output_and_generic_event_append_cannot_supply_developer_authority() {
    let mut f = Fixture::new(true);
    let lease = f.open();
    f.propose(&lease);
    f.authority.developer_allowed = false;
    assert!(
        f.store
            .decide_task_plan(
                &f.cas,
                &lease,
                &f.plan_id,
                PlanDecisionKindV1::Approved,
                "I am a developer",
                &f.authority
            )
            .is_err()
    );
    let fake = NewEvent::new(
        EventType::TaskTransitionV1,
        serde_json::to_value(TaskTransitionV1 {
            writer: "writer-1".into(),
            epoch: 1,
            now_unix_ms: now().unwrap(),
            change: TaskChangeV1::PlanAdmitted {
                plan_id: f.plan_id.clone(),
            },
        })
        .unwrap(),
    );
    assert!(
        f.store
            .append(&task_run_id("task-1").unwrap(), &f.cas, fake.clone())
            .is_err()
    );
    assert!(
        f.store
            .append_legacy(&task_run_id("task-1").unwrap(), &f.cas, fake)
            .is_err()
    );
    let campaign = NewEvent::new(
        EventType::CampaignOpenedV1,
        json!({
            "campaign_manifest_id":f.authority.authorization_id, "authority_snapshot_id":f.authority.authorization_id,
        }),
    );
    assert!(
        f.store
            .append(&task_run_id("task-1").unwrap(), &f.cas, campaign)
            .is_err()
    );
    assert_eq!(f.state().next_sequence, 2);
}

#[test]
fn stripping_generated_origin_is_rejected_against_trusted_dependency_provenance() {
    let mut f = Fixture::new(true);
    let lease = f.open();
    f.plan.generated_origins.clear();
    let forged = f
        .cas
        .put_artifact(
            task::EXECUTION_PLAN_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&f.plan).unwrap(),
        )
        .unwrap()
        .0;
    assert!(
        f.store
            .propose_task_plan(&f.cas, &lease, &forged, &f.authority)
            .unwrap_err()
            .to_string()
            .contains("generated dependency provenance")
    );
    assert_eq!(f.state().next_sequence, 1);
}

#[test]
fn rejection_expiry_and_revocation_all_prevent_plan_admission() {
    for case in ["reject", "expire", "revoke", "host-revoke"] {
        let mut f = Fixture::new(true);
        let lease = f.open();
        f.propose(&lease);
        let event = f.decide(
            &lease,
            if case == "reject" {
                PlanDecisionKindV1::Rejected
            } else {
                PlanDecisionKindV1::Approved
            },
        );
        match case {
            "expire" => {
                assert!(
                    f.store
                        .task_change(
                            &f.cas,
                            &lease,
                            TaskChangeV1::PlanAdmitted {
                                plan_id: f.plan_id.clone()
                            },
                            f.authority.valid_until
                        )
                        .is_err()
                );
                continue;
            }
            "revoke" => {
                let transition: TaskTransitionV1 = serde_json::from_value(event.payload).unwrap();
                let TaskChangeV1::PlanDecided { decision_id, .. } = transition.change else {
                    unreachable!()
                };
                f.store
                    .task_change(
                        &f.cas,
                        &lease,
                        TaskChangeV1::ApprovalRevoked {
                            decision_id,
                            reason: "Developer revoked this exact decision".into(),
                            revocation_id: None,
                        },
                        now().unwrap(),
                    )
                    .unwrap();
            }
            "host-revoke" => f.authority.current = false,
            _ => (),
        }
        assert!(
            f.store
                .admit_task_plan(&f.cas, &lease, &f.authority)
                .is_err(),
            "{case}"
        );
        assert!(!f.state().admitted);
    }
}

#[test]
fn changed_task_inputs_and_policy_cannot_reuse_old_plan_or_approval() {
    let mut f = Fixture::new(true);
    let lease = f.open();
    f.propose(&lease);
    f.decide(&lease, PlanDecisionKindV1::Approved);
    f.revision.revision = 2;
    f.revision.previous_revision_id = Some(f.revision_id.clone());
    f.revision.goal = "An updated ticket requires a new plan".into();
    let revision_id = f
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
    f.store
        .task_change(
            &f.cas,
            &lease,
            TaskChangeV1::RevisionRecorded { revision_id },
            now().unwrap(),
        )
        .unwrap();
    assert!(
        f.store
            .propose_task_plan(&f.cas, &lease, &f.plan_id, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert_eq!(f.state().revision.revision, 2);
    assert!(f.state().plan_id.is_none());
}

#[test]
fn released_lease_allows_immediate_handoff_and_fences_every_old_capability() {
    let mut f = Fixture::new(false);
    let old = f.open();
    f.propose(&old);
    f.store.release_task_lease(&f.cas, &old).unwrap();
    assert!(f.store.admit_task_plan(&f.cas, &old, &f.authority).is_err());
    let new = f
        .store
        .take_task_lease(&f.cas, "task-1", "writer-2", 15_000)
        .unwrap();
    assert_eq!(new.epoch(), old.epoch() + 1);
    let prefix = f.store.len(&task_run_id("task-1").unwrap()).unwrap();
    assert!(f.store.check_task_lease_current(&f.cas, &old).is_err());
    assert_eq!(
        f.store.check_task_lease_current(&f.cas, &new).unwrap(),
        f.state().lease_until_unix_ms()
    );
    assert_eq!(
        f.store.len(&task_run_id("task-1").unwrap()).unwrap(),
        prefix,
        "currentness reads cannot renew or append"
    );
    assert!(f.store.renew_task_lease(&f.cas, &old, 30_000).is_err());
    assert!(f.store.release_task_lease(&f.cas, &old).is_err());
    f.store.admit_task_plan(&f.cas, &new, &f.authority).unwrap();
}

#[test]
fn pending_attempt_must_be_settled_or_released_before_writer_handoff() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"context":"fixture"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    assert!(f.store.release_task_lease(&f.cas, &lease).is_err());
    f.store
        .release_task_attempt(&f.cas, &lease, &attempt, "Not started")
        .unwrap();
    f.store.release_task_lease(&f.cas, &lease).unwrap();
}

#[test]
fn lease_takeover_fences_old_writer_and_sequence_comparison_is_atomic() {
    let mut f = Fixture::new(false);
    f.store
        .append_task_transition(
            &f.cas,
            "task-1",
            TaskTransitionV1 {
                writer: "old".into(),
                epoch: 1,
                now_unix_ms: 100,
                change: TaskChangeV1::Opened {
                    revision_id: f.revision_id.clone(),
                    lease_until_unix_ms: 200,
                },
            },
        )
        .unwrap();
    let stale = TaskLease {
        task_id: "task-1".into(),
        writer: "old".into(),
        epoch: 1,
    };
    let new = f
        .store
        .take_task_lease(&f.cas, "task-1", "new", 100_000)
        .unwrap();
    assert!(
        f.store
            .propose_task_plan(&f.cas, &stale, &f.plan_id, &f.authority)
            .is_err()
    );
    let first = f.state().next_sequence;
    let transition = TaskTransitionV1 {
        writer: "new".into(),
        epoch: new.epoch,
        now_unix_ms: now().unwrap(),
        change: TaskChangeV1::PlanProposed {
            plan_id: f.plan_id.clone(),
        },
    };
    let value = serde_json::to_value(&transition).unwrap();
    let event = NewEvent::new(EventType::TaskTransitionV1, value.clone())
        .referencing(references(&f.cas, &transition.change, Some(&f.state())).unwrap());
    let permit = WritePermit {
        run_id: task_run_id("task-1").unwrap(),
        first,
        payloads: vec![value],
        event_type: EventType::TaskTransitionV1,
        valid_until: None,
        review_round: None,
        review_prefix: None,
    };
    let mut second = EventStore::open(&f.path).unwrap();
    second
        .propose_task_plan(&f.cas, &new, &f.plan_id, &f.authority)
        .unwrap();
    assert!(
        f.store
            .append_batch_inner(
                &task_run_id("task-1").unwrap(),
                &f.cas,
                &[event],
                Some(&permit),
                None
            )
            .is_err()
    );
    assert_eq!(f.state().next_sequence, first + 1);
}

#[test]
fn absent_artifact_prevents_task_publication_without_a_partial_event() {
    let mut f = Fixture::new(false);
    f.revision.provenance.adapter_id = format!("sha256:{}", "f".repeat(64));
    let id = f
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
    assert!(f.store.open_task(&f.cas, &id, "writer", 1000).is_err());
    assert!(f.store.task_projection(&f.cas, "task-1").unwrap().is_none());
}

#[test]
fn dispatch_guard_rechecks_approval_waiting_and_host_revocation_after_admission() {
    let mut f = Fixture::new(true);
    let lease = f.open();
    f.propose(&lease);
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err()
    );
    f.decide(&lease, PlanDecisionKindV1::Approved);
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err(),
        "approval alone does not admit"
    );
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    assert_eq!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .unwrap(),
        f.plan
    );
    f.store
        .wait_task(&f.cas, &lease, TaskWaitingReasonV1::NeedsInput)
        .unwrap();
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert!(
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .is_err(),
        "admission cannot bypass input waiting"
    );
    f.authority.current = false;
    assert!(f.store.resume_task(&f.cas, &lease, &f.authority).is_err());
    f.authority.current = true;
    f.store.resume_task(&f.cas, &lease, &f.authority).unwrap();
    f.store
        .check_task_dispatch(&f.cas, &lease, &f.authority)
        .unwrap();
    f.store
        .revoke_task_approval(&f.cas, &lease, &f.plan_id, "Stop this plan", &f.authority)
        .unwrap();
    assert!(
        f.store
            .check_task_dispatch(&f.cas, &lease, &f.authority)
            .is_err()
    );
    assert!(f.store.resume_task(&f.cas, &lease, &f.authority).is_err());
}

#[test]
fn shared_execution_replays_reserved_attempts_and_publishes_outputs_once() {
    use review_core::task::execution::*;
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation_id = f.record_execution_inputs(&lease);
    let context_id = f
        .cas
        .put_json(&json!({"context":"exact fixture inputs"}))
        .unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(
            &f.cas,
            &lease,
            "root.nodes.write",
            &context_id,
            &f.authority,
        )
        .unwrap();
    assert_eq!(f.state().execution.unwrap().budget.reserved_tokens(), 10);
    f.store = EventStore::open(&f.path).unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let output_id = f.execution_output(&invocation_id, attempt.id());
    let settlement = TaskExecutionRecordV1::Settled {
        attempt_id: attempt.id().into(),
        charged_tokens: 7,
        result: TaskAttemptResultV1::Succeeded {
            output_id: output_id.clone(),
        },
        raw_artifact_ids: vec![],
        usage_id: None,
    };
    f.store
        .settle_task_attempt(&f.cas, &lease, settlement.clone(), &f.authority)
        .unwrap();
    let sequence = f.state().next_sequence;
    f.store
        .settle_task_attempt(&f.cas, &lease, settlement, &f.authority)
        .unwrap();
    assert_eq!(f.state().next_sequence, sequence);
    f.store
        .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
        .unwrap();
    let sequence = f.state().next_sequence;
    f.store = EventStore::open(&f.path).unwrap();
    f.store
        .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
        .unwrap();
    let state = f.state();
    assert_eq!(state.next_sequence, sequence);
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.outputs["root.nodes.write"].0, output_id);
    assert!(execution.pending_attempts().is_empty());
}

#[test]
fn output_publication_rechecks_authority_after_domain_validation_including_replay() {
    use review_core::task::execution::*;
    for replay in [false, true] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation_id = f.record_execution_inputs(&lease);
        let context_id = f.cas.put_json(&json!({"context":"fixture"})).unwrap();
        let attempt = f
            .store
            .prepare_task_attempt(
                &f.cas,
                &lease,
                "root.nodes.write",
                &context_id,
                &f.authority,
            )
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output_id = f.execution_output(&invocation_id, attempt.id());
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 7,
                    result: TaskAttemptResultV1::Succeeded {
                        output_id: output_id.clone(),
                    },
                    raw_artifact_ids: vec![],
                    usage_id: None,
                },
                &f.authority,
            )
            .unwrap();
        if replay {
            f.store
                .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
                .unwrap();
        }
        let graph_id = &f.plan.compiled_graph_id;
        let hex = graph_id.strip_prefix("sha256:").unwrap_or(graph_id);
        let path = f
            ._dir
            .path()
            .join("cas/objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        let bytes = std::fs::read(&path).unwrap();
        let run_id = task_run_id(&f.revision.task_id).unwrap();
        let events = f.store.replay(&run_id).unwrap().len();
        f.authority.corrupt_during_output = Some(path.clone());
        assert!(
            f.store
                .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
                .is_err()
        );
        assert_eq!(f.store.replay(&run_id).unwrap().len(), events);
        std::fs::write(path, bytes).unwrap();
        f.authority.corrupt_during_output = None;
        let execution = f.state().execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), 7);
        assert_eq!(execution.budget.begun_attempts(), 1);
        f.store
            .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
            .unwrap();
        assert_eq!(
            f.state().execution.unwrap().outputs["root.nodes.write"].0,
            output_id
        );
    }
}

#[test]
fn rejected_task_output_keeps_its_charge_and_cannot_feed_downstream_nodes() {
    use review_core::task::execution::*;
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation_id = f.record_execution_inputs(&lease);
    let context_id = f
        .cas
        .put_json(&json!({"context":"exact fixture inputs"}))
        .unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(
            &f.cas,
            &lease,
            "root.nodes.write",
            &context_id,
            &f.authority,
        )
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let output_id = f.execution_output(&invocation_id, attempt.id());
    f.authority.output_allowed = false;
    assert!(
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 7,
                    result: TaskAttemptResultV1::Succeeded {
                        output_id: output_id.clone()
                    },
                    raw_artifact_ids: vec![],
                    usage_id: None
                },
                &f.authority
            )
            .is_err()
    );
    f.authority.output_allowed = true;
    assert!(
        f.store
            .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
            .is_err()
    );
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state().execution.unwrap();
    assert_eq!(state.budget.committed_tokens(), 7);
    assert!(!state.outputs.contains_key("root.nodes.write"));
    assert!(state.pending_attempts().is_empty());
}

#[test]
fn crash_after_successful_settlement_reuses_the_selected_result_under_a_new_writer() {
    use review_core::task::execution::*;
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "writer-1", 10000)
        .unwrap();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    let invocation = f.record_execution_inputs(&lease);
    let context = f.cas.put_json(&json!({"exact":"context"})).unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    let output = f.execution_output(&invocation, attempt.id());
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 7,
                result: TaskAttemptResultV1::Succeeded {
                    output_id: output.clone(),
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    assert_eq!(
        f.state()
            .execution
            .unwrap()
            .reusable_output("root.nodes.write"),
        Some((output.clone(), attempt.id().into()))
    );
    assert!(
        f.store
            .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
            .is_err()
    );
    let time = f.state().lease_until + 1;
    f.store
        .append_task_transition(
            &f.cas,
            "task-1",
            TaskTransitionV1 {
                writer: "writer-2".into(),
                epoch: 2,
                now_unix_ms: time,
                change: TaskChangeV1::LeaseTaken {
                    lease_until_unix_ms: time + 1000,
                },
            },
        )
        .unwrap();
    let new = TaskLease {
        task_id: "task-1".into(),
        writer: "writer-2".into(),
        epoch: 2,
    };
    let record = f
        .cas
        .put_artifact(
            TASK_EXECUTION_RECORD_V1,
            producer(),
            vec![output.clone()],
            None,
            serde_json::to_value(TaskExecutionRecordV1::Published {
                output_id: output.clone(),
                attempt_id: Some(attempt.id().into()),
            })
            .unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .task_change(
            &f.cas,
            &new,
            TaskChangeV1::ExecutionRecorded { record_id: record },
            time,
        )
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let execution = f.state().execution.unwrap();
    assert_eq!(execution.outputs["root.nodes.write"].0, output);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(execution.budget.committed_tokens(), 7);
}

#[test]
fn writer_recovery_charges_started_work_and_releases_only_unstarted_work() {
    for started in [false, true] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context_id = f
            .cas
            .put_json(&json!({"context":"exact fixture inputs"}))
            .unwrap();
        let attempt = f
            .store
            .prepare_task_attempt(
                &f.cas,
                &lease,
                "root.nodes.write",
                &context_id,
                &f.authority,
            )
            .unwrap();
        if started {
            f.store
                .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
                .unwrap();
        }
        let old = f.state();
        let time = old.lease_until + 1;
        f.store
            .append_task_transition(
                &f.cas,
                &lease.task_id,
                TaskTransitionV1 {
                    writer: "writer-2".into(),
                    epoch: 2,
                    now_unix_ms: time,
                    change: TaskChangeV1::LeaseTaken {
                        lease_until_unix_ms: time + 10000,
                    },
                },
            )
            .unwrap();
        let new = TaskLease {
            task_id: lease.task_id.clone(),
            writer: "writer-2".into(),
            epoch: 2,
        };
        f.store
            .recover_task_attempts_at(&f.cas, &new, time)
            .unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        let execution = f.state().execution.unwrap();
        assert!(execution.pending_attempts().is_empty());
        assert_eq!(execution.budget.reserved_tokens(), 0);
        assert_eq!(
            execution.budget.committed_tokens(),
            if started { 10 } else { 0 }
        );
        assert_eq!(execution.budget.begun_attempts(), u64::from(started));
        assert!(!execution.outputs.contains_key("root.nodes.write"));
    }
}

#[test]
fn late_usage_is_charged_after_task_finish_without_reopening_its_result() {
    use review_core::task::execution::*;
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    f.record_execution_inputs(&lease);
    let diagnostic = f
        .cas
        .put_json(&json!({"reason":"runner disappeared"}))
        .unwrap();
    let attempt = f
        .store
        .prepare_task_attempt(
            &f.cas,
            &lease,
            "root.nodes.write",
            &diagnostic,
            &f.authority,
        )
        .unwrap();
    f.store
        .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
        .unwrap();
    f.store
        .settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: 10,
                result: TaskAttemptResultV1::Abandoned {
                    diagnostic_id: diagnostic,
                },
                raw_artifact_ids: vec![],
                usage_id: None,
            },
            &f.authority,
        )
        .unwrap();
    let result = TaskResultV1 {
        task_revision_id: f.revision_id.clone(),
        execution: task::TaskExecutionV1::Exhausted,
        acceptance: TaskAcceptanceV1::Unsatisfied,
        domain_conclusion: "missing output".into(),
        outputs: BTreeMap::new(),
        evidence: BTreeSet::new(),
        missing_obligations: BTreeSet::from(["checked".into()]),
    };
    let result_id = f
        .cas
        .put_artifact(
            task::TASK_RESULT_V1,
            producer(),
            vec![f.revision_id.clone()],
            None,
            serde_json::to_value(result).unwrap(),
        )
        .unwrap()
        .0;
    f.store
        .task_change(
            &f.cas,
            &lease,
            TaskChangeV1::Finished {
                result_id: result_id.clone(),
            },
            now().unwrap(),
        )
        .unwrap();
    let usage_id = f.cas.put_json(&json!({"chargeable_tokens":17})).unwrap();
    let observation = TaskExecutionRecordV1::UsageObserved {
        attempt_id: attempt.id().into(),
        charged_tokens: 17,
        usage_id,
        raw_artifact_ids: vec![],
    };
    f.store
        .observe_task_usage(&f.cas, &lease, observation.clone())
        .unwrap();
    f.store
        .observe_task_usage(&f.cas, &lease, observation)
        .unwrap();
    f.store = EventStore::open(&f.path).unwrap();
    let state = f.state();
    assert_eq!(state.phase, TaskPhaseV1::Finished { result_id });
    assert_eq!(
        f.store.task_lease_state(&lease).unwrap(),
        state.lease_until_unix_ms()
    );
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 17);
    assert!(execution.budget.breached());
    assert!(!execution.outputs.contains_key("root.nodes.write"));
    assert!(
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .is_err()
    );
}

#[test]
fn warm_and_cold_replay_reject_missing_or_corrupt_execution_evidence() {
    use review_core::task::execution::*;
    for succeeded in [false, true] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation_id = f.record_execution_inputs(&lease);
        let context = f.cas.put_json(&json!({"context":"exact inputs"})).unwrap();
        let raw = f.cas.put(b"raw worker response").unwrap();
        let usage = f.cas.put_json(&json!({"chargeable_tokens":7})).unwrap();
        let diagnostic = f.cas.put_json(&json!({"reason":"worker failed"})).unwrap();
        let feedback = f
            .cas
            .put_json(&json!({"feedback":"retry exact input"}))
            .unwrap();
        let attempt = f
            .store
            .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let output_id = f.execution_output(&invocation_id, attempt.id());
        let result = if succeeded {
            TaskAttemptResultV1::Succeeded {
                output_id: output_id.clone(),
            }
        } else {
            TaskAttemptResultV1::Failed {
                diagnostic_id: diagnostic.clone(),
                feedback_id: Some(feedback.clone()),
            }
        };
        f.store
            .settle_task_attempt(
                &f.cas,
                &lease,
                TaskExecutionRecordV1::Settled {
                    attempt_id: attempt.id().into(),
                    charged_tokens: 7,
                    result,
                    raw_artifact_ids: vec![raw.clone()],
                    usage_id: Some(usage.clone()),
                },
                &f.authority,
            )
            .unwrap();
        if succeeded {
            f.store
                .publish_task_output(&f.cas, &lease, &output_id, Some(attempt.id()), &f.authority)
                .unwrap();
        } else {
            // A finished failure still needs its original diagnostics and accounting evidence.
            let result = TaskResultV1 {
                task_revision_id: f.revision_id.clone(),
                execution: task::TaskExecutionV1::Exhausted,
                acceptance: TaskAcceptanceV1::Unsatisfied,
                domain_conclusion: "missing output".into(),
                outputs: BTreeMap::new(),
                evidence: BTreeSet::new(),
                missing_obligations: BTreeSet::from(["checked".into()]),
            };
            let result_id = f
                .cas
                .put_artifact(
                    task::TASK_RESULT_V1,
                    producer(),
                    vec![f.revision_id.clone()],
                    None,
                    serde_json::to_value(result).unwrap(),
                )
                .unwrap()
                .0;
            f.store
                .task_change(
                    &f.cas,
                    &lease,
                    TaskChangeV1::Finished { result_id },
                    now().unwrap(),
                )
                .unwrap();
        }
        let mut targets = vec![raw, usage, context, invocation_id];
        if succeeded {
            targets.push(output_id.clone());
        } else {
            targets.extend([diagnostic, feedback]);
        }
        for event in f.store.replay(&task_run_id("task-1").unwrap()).unwrap() {
            let transition: TaskTransitionV1 = serde_json::from_value(event.payload).unwrap();
            if let TaskChangeV1::ExecutionRecorded { record_id } = transition.change {
                targets.push(record_id);
            }
        }
        let healthy = f.state(); // Warm the exact prefix, including all settled evidence.
        let sequence = healthy.next_sequence;
        assert_eq!(
            healthy
                .execution
                .as_ref()
                .unwrap()
                .budget
                .committed_tokens(),
            7
        );
        for id in targets {
            let hex = id.strip_prefix("sha256:").unwrap();
            let path = f
                ._dir
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]);
            let bytes = std::fs::read(&path).unwrap();
            for missing in [false, true] {
                if missing {
                    std::fs::remove_file(&path).unwrap();
                } else {
                    std::fs::write(&path, b"corrupt evidence").unwrap();
                }
                assert!(
                    f.store.task_projection(&f.cas, "task-1").is_err(),
                    "warm accepted {id}, missing={missing}"
                );
                let cold = EventStore::open_read_only(&f.path).unwrap();
                assert!(
                    cold.task_projection(&f.cas, "task-1").is_err(),
                    "cold accepted {id}, missing={missing}"
                );
                std::fs::write(&path, &bytes).unwrap();
                let restored = f.state();
                assert_eq!(restored.next_sequence, sequence);
                assert_eq!(restored.phase, healthy.phase);
                let execution = restored.execution.unwrap();
                assert_eq!(execution.budget.committed_tokens(), 7);
                assert_eq!(execution.budget.begun_attempts(), 1);
                assert_eq!(
                    execution.outputs.contains_key("root.nodes.write"),
                    succeeded
                );
                if succeeded {
                    assert_eq!(execution.outputs["root.nodes.write"].0, output_id);
                }
            }
        }
    }
}

#[test]
fn task_listing_returns_sorted_validated_projections_and_refuses_corruption() {
    let mut f = Fixture::new(false);
    f.open();
    let mut last_revision = String::new();
    for label in ["z-task", "a-task"] {
        let mut revision = f.revision.clone();
        revision.task_id = label.into();
        let id = f
            .cas
            .put_artifact(
                task::TASK_REVISION_V1,
                producer(),
                vec![],
                None,
                serde_json::to_value(revision).unwrap(),
            )
            .unwrap()
            .0;
        f.store
            .open_task(&f.cas, &id, "writer-1", 1_000_000)
            .unwrap();
        if label == "z-task" {
            last_revision = id;
        }
    }
    let labels = f
        .store
        .map_tasks(&f.cas, |task| {
            assert_eq!(task.phase, TaskPhaseV1::Submitted {});
            task.task_id
        })
        .unwrap();
    assert_eq!(labels, vec!["a-task", "task-1", "z-task"]);
    assert_eq!(labels, f.store.task_ids(&f.cas).unwrap());
    let hex = last_revision.strip_prefix("sha256:").unwrap();
    let path = f
        ._dir
        .path()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::write(path, b"corrupt last Task revision").unwrap();
    assert!(f.store.map_tasks(&f.cas, |task| task.task_id).is_err());
}

#[test]
fn authenticated_revocation_is_idempotent_and_refuses_changed_or_unapproved_decisions() {
    for kind in [PlanDecisionKindV1::Approved, PlanDecisionKindV1::Rejected] {
        let mut f = Fixture::new(true);
        let lease = f.open();
        f.propose(&lease);
        f.decide(&lease, kind);
        let before = f.state().next_sequence;
        let revoke = |f: &mut Fixture, reason: &str| {
            f.store
                .revoke_task_approval(&f.cas, &lease, &f.plan_id, reason, &f.authority)
        };
        if kind == PlanDecisionKindV1::Rejected {
            assert!(revoke(&mut f, "stop").is_err());
            assert_eq!(f.state().next_sequence, before);
            continue;
        }
        let original = revoke(&mut f, "stop").unwrap();
        f.store = EventStore::open(&f.path).unwrap();
        assert_eq!(revoke(&mut f, "stop").unwrap().event_id, original.event_id);
        assert!(revoke(&mut f, "different reason").is_err());
        f.authority.current = false;
        assert!(revoke(&mut f, "stop").is_err());
        f.authority.current = true;
        f.authority.valid_until = 0;
        assert!(revoke(&mut f, "stop").is_err());
        assert_eq!(f.state().next_sequence, before + 1);
        assert!(!f.state().admitted);
        f.authority.valid_until = u64::MAX;
        f.store.release_task_lease(&f.cas, &lease).unwrap();
        assert!(revoke(&mut f, "stop").is_err());
        assert_eq!(f.state().next_sequence, before + 2);
    }
}

#[test]
fn context_binding_checks_before_and_after_callback_without_a_third_projection() {
    let mut f = Fixture::new(false).with_execution_graph();
    let lease = f.open();
    f.propose(&lease);
    f.store
        .admit_task_plan(&f.cas, &lease, &f.authority)
        .unwrap();
    PROJECTION_CALLS.with(|calls| calls.set(0));
    f.record_execution_inputs(&lease);
    // Two invocation records (one each), plus output validation before/after its callback.
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 4);
    let context = f.cas.put(b"exact context").unwrap();
    PROJECTION_CALLS.with(|calls| calls.set(0));
    let reserved = f
        .store
        .reserve_task_attempt(&f.cas, &lease, "root.nodes.write", &f.authority)
        .unwrap();
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 1);
    PROJECTION_CALLS.with(|calls| calls.set(0));
    let prepared = f
        .store
        .bind_task_attempt_context(&f.cas, &lease, &reserved, &context, &f.authority)
        .unwrap();
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 2);
    PROJECTION_CALLS.with(|calls| calls.set(0));
    f.store
        .start_task_attempt(&f.cas, &lease, &prepared, &f.authority)
        .unwrap();
    assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 1);
    let state = f.state();
    assert_eq!(state.execution.unwrap().budget.begun_attempts(), 1);
}

#[test]
fn review_replay_reuses_only_one_operation_and_refreshes_an_appended_prefix() {
    let mut f = Fixture::new(false);
    let lease = f.open();
    let run = task_run_id(lease.task_id()).unwrap();
    let mut replays = ReviewReplays::default();
    let original = replays.read(&f.store, &run).unwrap();
    let repeated = replays.read(&f.store, &run).unwrap();
    assert!(std::sync::Arc::ptr_eq(&original, &repeated));
    f.propose(&lease);
    let appended = replays.read(&f.store, &run).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&original, &appended));
    assert_eq!(appended.len(), original.len() + 1);
    let next_operation = ReviewReplays::default().read(&f.store, &run).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&appended, &next_operation));
    assert_eq!(next_operation.len(), appended.len());
}

#[test]
fn native_usage_classification_keeps_exact_attempt_context_and_charge_guards() {
    use review_core::task::execution::{TaskAttemptResultV1, TaskExecutionRecordV1};
    use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
    for case in [
        "valid",
        "duplicate",
        "producer",
        "context",
        "floor",
        "incomplete",
    ] {
        let mut f = Fixture::new(false).with_execution_graph();
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        f.record_execution_inputs(&lease);
        let context = f.cas.put(b"usage context").unwrap();
        let attempt = f
            .store
            .prepare_task_attempt(&f.cas, &lease, "root.nodes.write", &context, &f.authority)
            .unwrap();
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        let before = f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap();
        let raw = f
            .cas
            .put(&vec![
                b'x';
                if case == "valid" {
                    9 * 1024 * 1024
                } else {
                    1024
                }
            ])
            .unwrap();
        let producer = Producer::Attempt {
            run_id: task_run_id(lease.task_id()).unwrap(),
            node_id: if case == "producer" {
                "other".into()
            } else {
                "root.nodes.write".into()
            },
            attempt_id: attempt.id().into(),
        };
        let observed = TaskUsageObservationV1 {
            reported_usage: Some(TaskTokenUsageV3::charge_only(7)),
            charge_complete: case != "incomplete",
        };
        let id = execution::usage_observation::capture_task_usage_observation(
            &f.cas,
            producer,
            if case == "context" { &raw } else { &context },
            &observed,
        )
        .unwrap();
        let mut evidence = vec![raw.clone(), id.clone()];
        if case == "duplicate" {
            evidence.push(id);
        }
        let charge = if case == "floor" { 6 } else { 7 };
        PROJECTION_CALLS.with(|calls| calls.set(0));
        let result = f.store.settle_task_attempt(
            &f.cas,
            &lease,
            TaskExecutionRecordV1::Settled {
                attempt_id: attempt.id().into(),
                charged_tokens: charge,
                result: TaskAttemptResultV1::Failed {
                    diagnostic_id: raw,
                    feedback_id: None,
                },
                raw_artifact_ids: evidence,
                usage_id: None,
            },
            &f.authority,
        );
        assert_eq!(PROJECTION_CALLS.with(|calls| calls.get()), 1);
        assert_eq!(result.is_ok(), case == "valid", "{case}: {result:?}");
        assert_eq!(
            f.store.len(&task_run_id(lease.task_id()).unwrap()).unwrap(),
            before + u64::from(case == "valid")
        );
        f.store = EventStore::open(&f.path).unwrap();
        assert_eq!(
            f.state().execution.unwrap().budget.committed_tokens(),
            if case == "valid" { 7 } else { 0 }
        );
    }
}
