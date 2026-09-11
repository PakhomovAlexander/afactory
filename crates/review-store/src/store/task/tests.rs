use super::*;
use review_core::Producer;
use review_core::task::plan::PlanDependencyV1;

struct Authority {
    generated: Vec<GeneratedOriginV1>,
    authorization_id: String,
    developer_allowed: bool,
    current: bool,
    valid_until: u64,
}

impl TaskAuthority for Authority {
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
                Some(&permit)
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
