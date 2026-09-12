//! Provider probes bind their own policy and Attempt, without borrowing a business slot.
use super::*;
use review_core::BrokerCredentialModeV1;
use review_core::task::provider::*;
use review_graph::task::{CapturedProviderProbe, OperatorAttemptCost};

const OTHER_SLOT: &str = "root.slots.other";
const PROBE: &str = "root.providers.admit0";
const DEPENDENCY: &str = "af/provider-probe-0";

struct ProbeAuthority;
impl TaskAuthority for ProbeAuthority {
    fn validate_plan(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
    ) -> Result<Vec<GeneratedOriginV1>, String> {
        BrokerAuthority.validate_plan(cas, task, plan)
    }
    fn authorize_decision(
        &self,
        task: &TaskRevisionV1,
        actor: &str,
        decision: PlanDecisionKindV1,
    ) -> Result<DeveloperGrant, String> {
        BrokerAuthority.authorize_decision(task, actor, decision)
    }
    fn authorization_current(&self, decision: &PlanDecisionV1) -> Result<(), String> {
        BrokerAuthority.authorization_current(decision)
    }
    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        BrokerAuthority.validate_result(cas, task, result)
    }
    fn validate_broker_binding(
        &self,
        cas: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        binding: &TaskBrokerBindingV1,
    ) -> Result<(), String> {
        let TaskBrokerTargetV1::ProviderAdmission { probe_policy_id } = &binding.target else {
            return Err("Provider fixture never grants business Worker authority".into());
        };
        let policy: TaskProviderProbePolicyV1 =
            payload(cas, probe_policy_id, TASK_PROVIDER_PROBE_POLICY_V1)
                .map_err(|e| e.to_string())?;
        policy.validate()?;
        if binding.operations != policy.operations {
            return Err("Provider operations differ from captured probe authority".into());
        }
        Ok(())
    }
}

struct ProbeFixture {
    f: Fixture,
    policy: TaskProviderProbePolicyV1,
    policy_id: String,
}

fn business_fixture() -> Fixture {
    let mut f = Fixture::new(false);
    f.revision.limits.verification.attempts = 2;
    f.revision.limits.verification.wall_ms = 2000;
    f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
    f.plan.task_revision_id = f.revision_id.clone();
    f.plan.limits = f.revision.limits.clone();
    let mut pipeline: task::pipeline::PipelineDefinitionV1 =
        payload(&f.cas, &f.plan.pipeline_id, task::PIPELINE_V1).unwrap();
    pipeline
        .slots
        .insert("other".into(), pipeline.slots["author"].clone());
    let mut other = pipeline.nodes[0].clone();
    other.id = "other".into();
    other.operator = task::pipeline::TaskOperatorV1::Worker {
        slot: "other".into(),
    };
    pipeline.nodes.push(other);
    let (id, envelope) = f
        .cas
        .put_artifact(
            task::PIPELINE_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(pipeline).unwrap(),
        )
        .unwrap();
    f.plan.pipeline_id = id.clone();
    let dependency = f.plan.dependencies.get_mut("builtin/document").unwrap();
    dependency.artifact_id = id;
    dependency.content_digest = envelope.content_id;
    f.with_execution_graph()
}

impl ProbeFixture {
    fn new() -> Self {
        Self::install(business_fixture())
    }

    fn install(mut f: Fixture) -> Self {
        let mut graph: CompiledTask =
            payload(&f.cas, &f.plan.compiled_graph_id, "af/CompiledTask@1").unwrap();
        let business_policy = f.cas.put_json(&json!([policy()])).unwrap();
        let package = f.cas.put_json(&json!({"fixture_worker":"model"})).unwrap();
        let execution = WorkerExecutionV1::Model {
            provider: "fixture".into(),
            provider_kind: "fixture".into(),
            principal_id: "fixture-account".into(),
            model: "fixture-model".into(),
            effort: "high".into(),
        };
        f.plan.bindings = graph
            .slots
            .keys()
            .map(|slot| {
                (
                    slot.clone(),
                    EffectiveWorkerBindingV1 {
                        package_digest: package.clone(),
                        package_artifact_id: package.clone(),
                        execution: execution.clone(),
                        invocation_policy_id: business_policy.clone(),
                    },
                )
            })
            .collect();
        let mut operation = policy();
        operation.name = "probe".into();
        operation.method = "probe-ok".into();
        operation.max_usage = 3;
        operation.max_calls = 1;
        let policy = TaskProviderProbePolicyV1 {
            authority_policy_id: f.revision.authority.policy_id.clone(),
            execution,
            credential_mode: BrokerCredentialModeV1::Brokered,
            probe_protocol: TaskProviderProbeProtocolV1::OkV1,
            operations: vec![operation],
        };
        let (policy_id, envelope) = f
            .cas
            .put_artifact(
                TASK_PROVIDER_PROBE_POLICY_V1,
                producer(),
                policy
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&policy).unwrap(),
            )
            .unwrap();
        f.plan.dependencies.insert(
            DEPENDENCY.into(),
            PlanDependencyV1 {
                name: DEPENDENCY.into(),
                artifact_id: policy_id.clone(),
                content_digest: envelope.content_id,
            },
        );
        let probes = f
            .plan
            .bindings
            .keys()
            .map(|slot| {
                (
                    slot.clone(),
                    CapturedProviderProbe {
                        policy_id: policy_id.clone(),
                        policy: policy.clone(),
                    },
                )
            })
            .collect();
        graph
            .install_provider_admission_with_probes(
                &f.plan.bindings,
                &OperatorAttemptCost {
                    tokens: 3,
                    wall_ms: 60_000,
                },
                &probes,
            )
            .unwrap();
        // Capture the verifier/probe reserve once, before opening the Task. Broker binding
        // must keep this admission allowance even though the business policy authorizes ten.
        f.revision.limits.verification.attempts = graph
            .allowances
            .values()
            .map(|a| a.verification_attempts)
            .sum();
        f.revision.limits.verification.wall_ms = graph
            .allowances
            .values()
            .map(|a| a.wall_ms_per_attempt * u64::from(a.verification_attempts))
            .sum();
        f.revision_id = put(&f.cas, task::TASK_REVISION_V1, &f.revision);
        f.plan.task_revision_id = f.revision_id.clone();
        f.plan.limits = f.revision.limits.clone();
        graph.budget(f.revision.limits.clone()).unwrap();
        f.plan.compiled_graph_id = f
            .cas
            .put_artifact(
                "af/CompiledTask@1",
                producer(),
                vec![policy_id.clone()],
                None,
                serde_json::to_value(graph).unwrap(),
            )
            .unwrap()
            .0;
        f.plan_id = put(&f.cas, task::EXECUTION_PLAN_V1, &f.plan);
        Self {
            f,
            policy,
            policy_id,
        }
    }

    /// Deliberately bypass trusted compiler validation to exercise the Store's own
    /// captured-policy and original-reservation checks with otherwise exact CAS closure.
    fn recapture_policy(&mut self) {
        let (id, envelope) = self
            .f
            .cas
            .put_artifact(
                TASK_PROVIDER_PROBE_POLICY_V1,
                producer(),
                self.policy
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(&self.policy).unwrap(),
            )
            .unwrap();
        self.policy_id = id.clone();
        let dependency = self.f.plan.dependencies.get_mut(DEPENDENCY).unwrap();
        dependency.artifact_id = id.clone();
        dependency.content_digest = envelope.content_id;
        let mut graph: CompiledTask = payload(
            &self.f.cas,
            &self.f.plan.compiled_graph_id,
            "af/CompiledTask@1",
        )
        .unwrap();
        let CompiledOperator::ProviderAdmissionBrokered {
            probe_policy_id, ..
        } = &mut graph.nodes.get_mut(PROBE).unwrap().operator
        else {
            unreachable!()
        };
        *probe_policy_id = id.clone();
        self.f.plan.compiled_graph_id = self
            .f
            .cas
            .put_artifact(
                "af/CompiledTask@1",
                producer(),
                vec![id],
                None,
                serde_json::to_value(graph).unwrap(),
            )
            .unwrap()
            .0;
    }

    fn prepare(&mut self) -> (TaskLease, PreparedTaskAttempt) {
        let f = &mut self.f;
        let lease = f.open();
        f.propose(&lease);
        f.store
            .admit_task_plan(&f.cas, &lease, &f.authority)
            .unwrap();
        let invocation = put(
            &f.cas,
            TASK_INVOCATION_V1,
            &TaskInvocationV1 {
                plan_id: f.plan_id.clone(),
                node: PROBE.into(),
                inputs: BTreeMap::new(),
            },
        );
        f.store
            .record_task_invocation(&f.cas, &lease, &invocation, &f.authority)
            .unwrap();
        let context = f
            .cas
            .put_json(&json!({"probe":"OK","policy":self.policy_id}))
            .unwrap();
        let attempt = f
            .store
            .prepare_task_attempt(&f.cas, &lease, PROBE, &context, &f.authority)
            .unwrap();
        (lease, attempt)
    }

    fn start(&mut self) -> (TaskLease, PreparedTaskAttempt) {
        let (lease, attempt) = self.prepare();
        let f = &mut self.f;
        f.store
            .start_task_attempt(&f.cas, &lease, &attempt, &f.authority)
            .unwrap();
        (lease, attempt)
    }

    fn bind(&mut self, lease: &TaskLease, attempt: &PreparedTaskAttempt) -> BoundTaskBroker {
        self.f
            .store
            .bind_task_broker(
                &self.f.cas,
                lease,
                attempt,
                HANDLE,
                &self.policy.operations,
                &ProbeAuthority,
            )
            .unwrap()
    }
}

#[test]
fn provider_probe_uses_its_own_started_attempt_context_and_original_allowance() {
    let mut p = ProbeFixture::new();
    let (lease, attempt) = p.prepare();
    let sequence = p.f.state().next_sequence;
    assert!(
        p.f.store
            .bind_task_broker(
                &p.f.cas,
                &lease,
                &attempt,
                HANDLE,
                &p.policy.operations,
                &ProbeAuthority
            )
            .is_err()
    );
    assert_eq!(p.f.state().next_sequence, sequence);
    p.f.store
        .start_task_attempt(&p.f.cas, &lease, &attempt, &p.f.authority)
        .unwrap();
    let bound = p.bind(&lease, &attempt);
    let binding = bound.binding();
    assert_eq!(
        binding.target,
        TaskBrokerTargetV1::ProviderAdmission {
            probe_policy_id: p.policy_id.clone()
        }
    );
    assert_eq!(binding.node, PROBE);
    assert_eq!(binding.lease.node_id, PROBE);
    assert_eq!(binding.lease.attempt_id, attempt.id());
    assert_eq!(binding.context_id, attempt.context_id());
    assert_eq!(binding.reservation_id, attempt.reservation().id);
    assert_eq!(attempt.reservation().tokens, 3);
    assert_eq!(
        p.f.state().execution.as_ref().unwrap().graph.allowances[NODE].tokens_per_attempt,
        10
    );
    assert!(!p.f.plan.bindings.contains_key(PROBE));
    let graph = &p.f.state().execution.unwrap().graph;
    assert!(
        matches!(&graph.nodes[PROBE].operator, CompiledOperator::ProviderAdmissionBrokered { bindings, probe_policy_id }
        if bindings == &BTreeSet::from([SLOT.into(), OTHER_SLOT.into()]) && probe_policy_id == &p.policy_id)
    );
    assert!(
        graph.nodes[NODE]
            .conditions
            .iter()
            .any(|c| c.source.node == PROBE)
    );
    assert!(
        graph.nodes["root.nodes.other"]
            .conditions
            .iter()
            .any(|c| c.source.node == PROBE)
    );
    assert!(
        p.f.store
            .check_task_broker_current(&p.f.cas, &bound, &BrokerAuthority)
            .is_err()
    );
    p.f.store
        .check_task_broker_current(&p.f.cas, &bound, &ProbeAuthority)
        .unwrap();
    let paid = receipt(&bound, 1, 2);
    assert_eq!(
        p.f.store
            .record_task_broker_receipt(&p.f.cas, &bound, &paid, &ProbeAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::Recorded
    );
    let execution = p.f.state().execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 2);
    assert_eq!(execution.budget.begun_attempts(), 1);
    let attempts = execution.attempt_accounting();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].reservation.node, PROBE);
    assert_eq!(attempts[0].reservation.tokens, 3);
}

#[test]
fn provider_binding_rejects_business_operations_and_checks_every_protected_execution() {
    for alteration in [
        "business operations",
        "oversized operations",
        "captured oversized policy",
        "foreign authority with exact dependency",
        "second binding",
        "missing dependency",
        "wrong dependency digest",
    ] {
        let mut p = ProbeFixture::new();
        let mut operations = p.policy.operations.clone();
        match alteration {
            "business operations" => operations = vec![policy()],
            "oversized operations" => operations[0].max_usage = 4,
            "captured oversized policy" => {
                p.policy.operations[0].max_usage = 4;
                operations = p.policy.operations.clone();
                p.recapture_policy();
            }
            "foreign authority with exact dependency" => {
                p.policy.authority_policy_id =
                    p.f.cas
                        .put_json(&json!({"foreign_authority":true}))
                        .unwrap();
                p.recapture_policy();
            }
            "second binding" => {
                let WorkerExecutionV1::Model { model, .. } =
                    &mut p.f.plan.bindings.get_mut(OTHER_SLOT).unwrap().execution
                else {
                    unreachable!()
                };
                *model = "unadmitted-model".into();
            }
            "missing dependency" => {
                p.f.plan.dependencies.remove(DEPENDENCY);
            }
            "wrong dependency digest" => {
                p.f.plan
                    .dependencies
                    .get_mut(DEPENDENCY)
                    .unwrap()
                    .content_digest = p.f.plan.engine_id.clone();
            }
            _ => unreachable!(),
        }
        p.f.plan_id = put(&p.f.cas, task::EXECUTION_PLAN_V1, &p.f.plan);
        if alteration == "wrong dependency digest" {
            let lease = p.f.open();
            let sequence = p.f.state().next_sequence;
            let error =
                p.f.store
                    .propose_task_plan(&p.f.cas, &lease, &p.f.plan_id, &p.f.authority)
                    .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Plan dependency content disagrees with its lock"),
                "{error}"
            );
            assert_eq!(p.f.state().next_sequence, sequence);
            assert!(p.f.state().execution.is_none());
            continue;
        }
        let (lease, attempt) = p.start();
        let sequence = p.f.state().next_sequence;
        let error =
            p.f.store
                .bind_task_broker(
                    &p.f.cas,
                    &lease,
                    &attempt,
                    HANDLE,
                    &operations,
                    &ProbeAuthority,
                )
                .unwrap_err();
        assert!(
            matches!(error, StoreError::Conflict(_)),
            "{alteration}: {error}"
        );
        let state = p.f.state();
        assert_eq!(state.next_sequence, sequence, "{alteration}");
        assert_eq!(
            state.execution.unwrap().budget.reserved_tokens(),
            3,
            "{alteration}"
        );
    }
}

#[test]
fn provider_probe_target_cannot_be_replaced_by_a_worker_or_foreign_policy_in_replay() {
    let mut p = ProbeFixture::new();
    let (lease, attempt) = p.start();
    let before = p.f.state();
    let bound = p.bind(&lease, &attempt);
    let event =
        p.f.store
            .replay(&task_run_id(&p.f.revision.task_id).unwrap())
            .unwrap()
            .pop()
            .unwrap();
    let mut foreign_policy = p.policy.clone();
    foreign_policy.operations[0].method = "other-probe".into();
    let foreign_id =
        p.f.cas
            .put_artifact(
                TASK_PROVIDER_PROBE_POLICY_V1,
                producer(),
                foreign_policy
                    .artifact_refs()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                None,
                serde_json::to_value(foreign_policy).unwrap(),
            )
            .unwrap()
            .0;
    for target in [
        TaskBrokerTargetV1::Worker {
            slot: SLOT.into(),
            invocation_policy_id: p.f.plan.bindings[SLOT].invocation_policy_id.clone(),
        },
        TaskBrokerTargetV1::ProviderAdmission {
            probe_policy_id: foreign_id,
        },
    ] {
        let mut binding = bound.binding().clone();
        binding.target = target;
        let record =
            p.f.cas
                .put_artifact(
                    TASK_BROKER_BINDING_V1,
                    Producer::KernelOperation {
                        run_id: event.run_id.clone(),
                        node_id: None,
                        operation_id: "task-broker@1".into(),
                    },
                    binding
                        .artifact_refs()
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    None,
                    serde_json::to_value(binding).unwrap(),
                )
                .unwrap()
                .0;
        let mut changed = event.clone();
        changed.artifact_refs = vec![record.clone()];
        changed.payload["record_id"] = json!(record);
        assert!(apply_event(&p.f.cas, &mut before.clone(), &changed).is_err());
    }
    assert_eq!(p.f.state().next_sequence, before.next_sequence + 1);
}

#[test]
fn provider_probe_lease_preserves_captured_round_and_retains_late_paid_work() {
    let (f, round) = review::round::round_fixture();
    let mut p = ProbeFixture::install(f);
    let (lease, attempt) = p.start();
    let bound = p.bind(&lease, &attempt);
    assert_eq!(bound.binding().lease.campaign_id, round.campaign_id);
    assert_eq!(bound.binding().lease.round_event_id, round.round_event_id);
    assert_eq!(bound.binding().lease.node_id, PROBE);
    assert_ne!(bound.binding().lease.node_id, "reviewer");
    review::round::supersede(&p.f, &round);
    assert!(
        p.f.store
            .check_task_broker_current(&p.f.cas, &bound, &ProbeAuthority)
            .is_err()
    );
    let paid = receipt(&bound, 1, 3);
    let before = p.f.state().next_sequence;
    assert_eq!(
        p.f.store
            .record_task_broker_receipt(&p.f.cas, &bound, &paid, &ProbeAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::AuthorityRevoked
    );
    settle(&mut p.f, &lease, &attempt, 1);
    p.f.store = EventStore::open(&p.f.path).unwrap();
    let state = p.f.state();
    assert_eq!(state.next_sequence, before + 2);
    let execution = state.execution.unwrap();
    assert_eq!(execution.budget.committed_tokens(), 3);
    assert_eq!(execution.budget.begun_attempts(), 1);
    assert_eq!(
        execution.attempt_accounting()[0].reservation,
        *attempt.reservation()
    );
    assert_eq!(operations(&p.f)[0].1.receipt.outcome, Outcome::Revoked);
    assert_eq!(
        p.f.store
            .record_task_broker_receipt(&p.f.cas, &bound, &paid, &ProbeAuthority)
            .unwrap(),
        TaskBrokerReceiptDisposition::AuthorityRevoked
    );
    assert_eq!(p.f.state().next_sequence, before + 2);
}
