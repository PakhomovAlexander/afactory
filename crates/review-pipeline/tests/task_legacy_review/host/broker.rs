use super::*;
use review_core::task::TaskRevisionV1;
use review_core::task::broker::{TaskBrokerBindingV1, TaskBrokerTargetV1};
use review_core::task::execution::TaskInvocationV1;
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_core::{BrokerCredentialModeV1, BrokerLeaseV1, BrokerOperationPolicyV1};
use review_graph::task::{CompiledOperator, CompiledTask, ReviewOperation};
use review_pipeline::task::TaskOperatorHost;
use review_pipeline::task::host::{TaskDomain, TaskModelBinding};
use review_pipeline::task::provider::ProviderTaskDomain;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};
use review_store::store::task::{TaskAuthority, TaskLease};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Model {
    mode: BrokerCredentialModeV1,
    calls: AtomicUsize,
}
impl WorkerModelAdapter for Model {
    fn credential_mode(&self) -> BrokerCredentialModeV1 {
        self.mode
    }
    fn provider_kind(&self) -> &'static str {
        "claude"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some(("claude-fixture".into(), "high".into()))
    }
    fn invoke(
        &self,
        _: &Cas,
        _: &std::path::Path,
        _: Vec<u8>,
        _: std::time::Duration,
        _: bool,
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("Brokered Review cannot fall back to ambient Provider invocation")
    }
    fn invoke_with_broker(
        &self,
        _: &Cas,
        _: &std::path::Path,
        _: Vec<u8>,
        _: std::time::Duration,
        _: bool,
        _: Option<&dyn review_broker::ExactBrokerClient>,
    ) -> ModelWorkerReturn {
        self.calls.fetch_add(1, Ordering::SeqCst);
        panic!("Provider admission has no captured Broker policy yet")
    }
}

struct IdentitySubstitution<'a> {
    inner: &'a Model,
    kind: &'static str,
    model: &'static str,
    effort: &'static str,
}
impl WorkerModelAdapter for IdentitySubstitution<'_> {
    fn credential_mode(&self) -> BrokerCredentialModeV1 {
        self.inner.credential_mode()
    }
    fn provider_kind(&self) -> &'static str {
        self.kind
    }
    fn model_settings(&self) -> Option<(String, String)> {
        Some((self.model.into(), self.effort.into()))
    }
    fn invoke(
        &self,
        cas: &Cas,
        cwd: &std::path::Path,
        input: Vec<u8>,
        timeout: std::time::Duration,
        schema: bool,
    ) -> ModelWorkerReturn {
        self.inner.invoke(cas, cwd, input, timeout, schema)
    }
}

pub(super) struct Captured {
    pub(super) compiler: LegacyReviewPlanCompiler,
    pub(super) task: TaskRevisionV1,
    pub(super) plan: ExecutionPlanV1,
    pub(super) plan_id: String,
    pub(super) graph: CompiledTask,
    pub(super) lease: TaskLease,
}
impl Captured {
    fn new(cas: &Cas, store: &mut EventStore) -> Self {
        Self::with_probe(cas, store, false)
    }

    pub(super) fn new_with_probe(cas: &Cas, store: &mut EventStore) -> Self {
        Self::with_probe(cas, store, true)
    }

    fn with_probe(cas: &Cas, store: &mut EventStore, probe: bool) -> Self {
        let definition = PIPELINE
            .replace("version = 2", "version = 4\n[gate]\nprovider=\"trusted_local\"\nrequired_isolation=\"none\"\nmode=\"ephemeral-write\"")
            .replace("runner = { program = \"/bin/true\" }", r#"package="fixture"
gated_by="gate"
execution={credential_mode="brokered",operations=[{name="inference",destination="fixture.test",method="respond",max_request_bytes=4096,max_response_bytes=4096,max_calls=1,max_usage=10}]}"#)
            + "\n[[nodes]]\nid=\"gate\"\nkind=\"gate\"\noutputs=[\"decision\"]\n[[checks]]\nname=\"required\"\nprogram=\"/bin/sh\"\nargs=[{value=\"-c\"},{value=\"exit 0\"}]\n";
        let round = capture::open_round_with_package(cas, store, &definition);
        let mut settings = plan::settings();
        settings.resources.uncapped_attempt_tokens = 2048;
        settings.provider_admission.tokens = 32;
        settings.executions.insert(
            "reviewer".into(),
            WorkerExecutionV1::Model {
                provider: "claude-personal".into(),
                provider_kind: "claude".into(),
                principal_id: "fixture-personal-account".into(),
                model: "claude-fixture".into(),
                effort: "high".into(),
            },
        );
        let round = CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap();
        let engine = cas.put(b"Broker forwarding fixture engine").unwrap();
        // Without a probe this is the shape `af review run` captures: a Brokered reviewer
        // behind plain Provider admission.
        let mut settings = plan::without_probes(settings);
        if probe {
            use review_core::task::provider::TaskProviderProbeProtocolV1;
            use review_pipeline::task::legacy_review::plan::ReviewProviderProbeSettingsV1;
            settings.provider_probes.insert(
                "reviewer".into(),
                ReviewProviderProbeSettingsV1 {
                    probe_protocol: TaskProviderProbeProtocolV1::OkV1,
                    operations: probe_operations(),
                },
            );
        }
        let compiler = LegacyReviewPlanCompiler::capture(cas, round, engine, settings).unwrap();
        let mut limits = capture::limits();
        limits.tokens = 100_000;
        let task = compiler
            .prepare_revision(cas, "broker-host-review", limits)
            .unwrap();
        let revision = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
        let (plan, compilation) = compiler.compile(cas, &revision).unwrap();
        let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &plan);
        let authority = CapturedTaskAuthority::for_legacy_review(
            &compiler,
            &plan::RefuseExecution,
            &NoTaskDeveloper,
        );
        let lease = store.open_task(cas, &revision, "developer", 60000).unwrap();
        store
            .propose_task_plan(cas, &lease, &plan_id, &authority)
            .unwrap();
        store.admit_task_plan(cas, &lease, &authority).unwrap();
        Self {
            compiler,
            task,
            plan,
            plan_id,
            graph: compilation.compilation.graph,
            lease,
        }
    }
    pub(super) fn models<'a>(
        &self,
        model: &'a dyn WorkerModelAdapter,
    ) -> BTreeMap<String, TaskModelBinding<'a>> {
        self.plan
            .bindings
            .iter()
            .map(|(slot, binding)| {
                (
                    slot.clone(),
                    TaskModelBinding {
                        binding: binding.clone(),
                        adapter: model,
                    },
                )
            })
            .collect()
    }
    fn invocation(&self, node: &str) -> TaskInvocationV1 {
        TaskInvocationV1 {
            plan_id: self.plan_id.clone(),
            node: node.into(),
            inputs: BTreeMap::new(),
        }
    }
    fn binding(&self, cas: &Cas) -> TaskBrokerBindingV1 {
        let (node, slot) = self
            .graph
            .nodes
            .iter()
            .find_map(|(node, value)| match &value.operator {
                CompiledOperator::ReviewDomain {
                    operation: ReviewOperation::Reviewer { slot },
                    ..
                } => Some((node, slot)),
                _ => None,
            })
            .unwrap();
        let round = self.compiler.round().binding();
        TaskBrokerBindingV1 {
            task_id: self.task.task_id.clone(),
            task_revision_id: self.plan.task_revision_id.clone(),
            plan_id: self.plan_id.clone(),
            invocation_id: cas.put(b"invocation identity").unwrap(),
            context_id: cas.put(b"context identity").unwrap(),
            attempt_id: "a".repeat(26),
            reservation_id: "reservation:0".into(),
            writer: "developer".into(),
            writer_epoch: self.lease.epoch(),
            node: node.clone(),
            target: TaskBrokerTargetV1::Worker {
                slot: slot.clone(),
                invocation_policy_id: self.plan.bindings[slot].invocation_policy_id.clone(),
            },
            lease: BrokerLeaseV1 {
                campaign_id: round.campaign_id,
                round_event_id: round.round_event_id,
                node_id: "reviewer".into(),
                attempt_id: "a".repeat(26),
                lease_epoch: self.lease.epoch(),
            },
            handle_id: "b".repeat(26),
            operations: vec![BrokerOperationPolicyV1 {
                name: "inference".into(),
                destination: "fixture.test".into(),
                method: "respond".into(),
                max_request_bytes: 4096,
                max_response_bytes: 4096,
                max_calls: 1,
                max_usage: 10,
            }],
        }
    }
}

fn probe_operations() -> Vec<BrokerOperationPolicyV1> {
    vec![BrokerOperationPolicyV1 {
        name: "capability".into(),
        destination: "probe.test".into(),
        method: "respond".into(),
        max_request_bytes: 128,
        max_response_bytes: 128,
        max_calls: 1,
        max_usage: 7,
    }]
}

#[test]
fn provider_v2_uses_only_fixed_context_and_its_separate_probe_target() {
    use review_runner::task::provider::{
        PROBE_INPUT, TASK_PROVIDER_CONTEXT_V2, TaskProviderContextV2,
    };
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new_with_probe(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::Brokered,
        calls: AtomicUsize::new(0),
    };
    let host = LegacyReviewTaskHost::new(
        &cas,
        SharedEventStore::new(&mut store),
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .unwrap();
    let (node, probe_id) = captured
        .graph
        .nodes
        .iter()
        .find_map(|(node, value)| match &value.operator {
            CompiledOperator::ProviderAdmissionBrokered {
                probe_policy_id, ..
            } => Some((node, probe_policy_id)),
            _ => None,
        })
        .unwrap();
    let input = captured.invocation(node);
    assert_eq!(
        host.broker_operations(&cas, &input).unwrap(),
        Some(probe_operations())
    );
    let context_id = host.prepare_context(&cas, &input, &[]).unwrap();
    let envelope = cas.get_artifact(&context_id).unwrap();
    assert_eq!(envelope.artifact_type, TASK_PROVIDER_CONTEXT_V2);
    let context: TaskProviderContextV2 = serde_json::from_value(envelope.payload).unwrap();
    context.validate().unwrap();
    assert_eq!(context.invocation, input);
    assert!(context.invocation.inputs.is_empty());
    assert_eq!(context.capability.probe_policy_id, *probe_id);
    assert_eq!(cas.get(&context.rendered_id).unwrap(), PROBE_INPUT);
    assert_eq!(envelope.input_artifacts, context.artifact_refs());
    assert_eq!(
        context.capability.execution,
        captured.plan.bindings.values().next().unwrap().execution
    );
    assert!(
        !String::from_utf8(cas.get(&context.rendered_id).unwrap())
            .unwrap()
            .contains("Captured instruction marker")
    );
    host.validate_context(&cas, &input, &[], &context_id)
        .unwrap();
    assert!(
        host.prepare_context(&cas, &input, &[context_id.clone()])
            .is_err()
    );
    let mut private_input = input.clone();
    private_input.inputs = captured.task.inputs.clone();
    assert!(host.prepare_context(&cas, &private_input, &[]).is_err());
    assert!(host.broker_operations(&cas, &private_input).is_err());

    let worker = captured.binding(&cas);
    assert_ne!(worker.operations, probe_operations());
    let mut binding = worker.clone();
    binding.node = node.clone();
    binding.lease.node_id = node.clone();
    binding.context_id = context_id;
    binding.target = TaskBrokerTargetV1::ProviderAdmission {
        probe_policy_id: probe_id.clone(),
    };
    binding.operations = probe_operations();
    // Synthetic identities exercise only domain policy; Store Started/lease admission is
    // covered separately and cannot be obtained from this constructed record.
    let authority =
        CapturedTaskAuthority::for_legacy_review(&captured.compiler, &host, &NoTaskDeveloper);
    authority
        .validate_broker_binding(&cas, &captured.task, &captured.plan, &binding)
        .unwrap();
    for mutation in 0..6 {
        let mut changed = binding.clone();
        match mutation {
            0 => changed.target = worker.target.clone(),
            1 => changed.operations = worker.operations.clone(),
            2 => changed.lease.node_id = worker.lease.node_id.clone(),
            3 => {
                changed.target = TaskBrokerTargetV1::ProviderAdmission {
                    probe_policy_id: worker.target.policy_id().into(),
                }
            }
            4 => changed.lease.round_event_id = "z".repeat(26),
            5 => changed.node = worker.node.clone(),
            _ => unreachable!(),
        }
        assert!(
            authority
                .validate_broker_binding(&cas, &captured.task, &captured.plan, &changed)
                .is_err(),
            "mutation {mutation}"
        );
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn provider_v2_refuses_missing_runtime_broker_and_changed_protected_identity() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new_with_probe(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::Brokered,
        calls: AtomicUsize::new(0),
    };
    let native = Model {
        mode: BrokerCredentialModeV1::TrustedUnsafe,
        calls: AtomicUsize::new(0),
    };
    let node = captured
        .graph
        .nodes
        .iter()
        .find(|(_, value)| {
            matches!(
                value.operator,
                CompiledOperator::ProviderAdmissionBrokered { .. }
            )
        })
        .unwrap()
        .0;
    let input = captured.invocation(node);
    let native_models = captured.models(&native);
    let native_domain = ProviderTaskDomain {
        graph: &captured.graph,
        models: &native_models,
        inner: &plan::RefuseExecution,
    };
    assert!(native_domain.prepare_context(&cas, &input, &[]).is_err());
    assert!(native_domain.broker_operations(&cas, &input).is_err());
    for (kind, selected_model, effort) in [
        ("other-provider", "claude-fixture", "high"),
        ("claude", "other-model", "high"),
        ("claude", "claude-fixture", "low"),
    ] {
        let adapter = IdentitySubstitution {
            inner: &model,
            kind,
            model: selected_model,
            effort,
        };
        let models = captured.models(&adapter);
        let domain = ProviderTaskDomain {
            graph: &captured.graph,
            models: &models,
            inner: &plan::RefuseExecution,
        };
        assert!(domain.prepare_context(&cas, &input, &[]).is_err());
        assert!(domain.broker_operations(&cas, &input).is_err());
    }
    for mutation in 0..7 {
        let mut models = captured.models(&model);
        let binding = &mut models.values_mut().next().unwrap().binding;
        match mutation {
            0 => binding.package_artifact_id = captured.plan_id.clone(),
            1 => binding.invocation_policy_id = captured.plan_id.clone(),
            _ => {
                let WorkerExecutionV1::Model {
                    provider,
                    provider_kind,
                    principal_id,
                    model,
                    effort,
                } = &mut binding.execution
                else {
                    unreachable!()
                };
                match mutation {
                    2 => *provider = "other-alias".into(),
                    3 => *provider_kind = "other-kind".into(),
                    4 => *principal_id = "other-account".into(),
                    5 => *model = "other-model".into(),
                    6 => *effort = "low".into(),
                    _ => unreachable!(),
                }
            }
        }
        let domain = ProviderTaskDomain {
            graph: &captured.graph,
            models: &models,
            inner: &plan::RefuseExecution,
        };
        assert!(
            domain.prepare_context(&cas, &input, &[]).is_err(),
            "mutation {mutation}"
        );
        assert!(
            domain.broker_operations(&cas, &input).is_err(),
            "mutation {mutation}"
        );
    }
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .unwrap();
    let authority =
        CapturedTaskAuthority::for_legacy_review(&captured.compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared, &cas, captured.lease.clone(), &authority, &host).unwrap();
    assert!(!runtime.execute().unwrap().complete());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(native.calls.load(Ordering::SeqCst), 0);
    assert!(
        runtime
            .projection()
            .unwrap()
            .execution
            .unwrap()
            .attempt_accounting()
            .iter()
            .any(|attempt| attempt.started && attempt.result.is_some())
    );
}

#[test]
fn captured_reviewer_broker_policy_is_exact_and_provider_admission_has_no_operations() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::Brokered,
        calls: AtomicUsize::new(0),
    };
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared,
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .unwrap();
    let models = captured.models(&model);
    let provider = ProviderTaskDomain {
        graph: &captured.graph,
        models: &models,
        inner: &host,
    };
    let authority =
        CapturedTaskAuthority::for_legacy_review(&captured.compiler, &provider, &NoTaskDeveloper);
    let binding = captured.binding(&cas);
    for (node, value) in &captured.graph.nodes {
        let expected = matches!(
            value.operator,
            CompiledOperator::ReviewDomain {
                operation: ReviewOperation::Reviewer { .. },
                ..
            }
        )
        .then(|| binding.operations.clone());
        assert_eq!(
            host.broker_operations(&cas, &captured.invocation(node))
                .unwrap(),
            expected
        );
        assert_eq!(
            provider
                .broker_operations(&cas, &captured.invocation(node))
                .unwrap(),
            expected
        );
    }
    // This checks the domain policy callback, not Store admission of these synthetic identities.
    authority
        .validate_broker_binding(&cas, &captured.task, &captured.plan, &binding)
        .unwrap();
    let changes: [fn(&mut TaskBrokerBindingV1); 14] = [
        |b| b.task_id = "another-task".into(),
        |b| b.task_revision_id = b.context_id.clone(),
        |b| b.plan_id = b.context_id.clone(),
        |b| b.node = "root.unknown".into(),
        |b| {
            if let TaskBrokerTargetV1::Worker { slot, .. } = &mut b.target {
                *slot = "root.other".into();
            }
        },
        |b| {
            if let TaskBrokerTargetV1::Worker {
                invocation_policy_id,
                ..
            } = &mut b.target
            {
                *invocation_policy_id = b.context_id.clone();
            }
        },
        |b| {
            b.writer_epoch += 1;
            b.lease.lease_epoch += 1;
        },
        |b| b.lease.campaign_id = "another-campaign".into(),
        |b| b.lease.round_event_id = "c".repeat(26),
        |b| b.lease.node_id = "another-reviewer".into(),
        |b| b.operations[0].destination = "another.test".into(),
        |b| b.operations[0].max_calls += 1,
        |b| b.operations[0].max_usage += 1,
        |b| b.operations.clear(),
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut changed = binding.clone();
        change(&mut changed);
        assert!(
            authority
                .validate_broker_binding(&cas, &captured.task, &captured.plan, &changed)
                .is_err(),
            "mutation {index}"
        );
    }
    let mut changed_plan = captured.plan.clone();
    changed_plan
        .bindings
        .get_mut(match &binding.target {
            TaskBrokerTargetV1::Worker { slot, .. } => slot,
            _ => unreachable!(),
        })
        .unwrap()
        .invocation_policy_id = binding.context_id.clone();
    assert!(
        authority
            .validate_broker_binding(&cas, &captured.task, &changed_plan, &binding)
            .is_err()
    );
    let provider_node = captured
        .graph
        .nodes
        .iter()
        .find(|(_, n)| matches!(n.operator, CompiledOperator::ProviderAdmission { .. }))
        .unwrap()
        .0;
    let mut provider_binding = binding.clone();
    provider_binding.node = provider_node.clone();
    assert!(
        provider
            .validate_broker_binding(&cas, &captured.task, &captured.plan, &provider_binding)
            .unwrap_err()
            .contains("no captured Broker operation policy")
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn brokered_provider_without_distinct_probe_authority_never_invokes_the_adapter() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::Brokered,
        calls: AtomicUsize::new(0),
    };
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .unwrap();
    let authority =
        CapturedTaskAuthority::for_legacy_review(&captured.compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared, &cas, captured.lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    assert!(!report.complete());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let state = runtime.projection().unwrap();
    assert!(
        state
            .execution
            .unwrap()
            .attempt_accounting()
            .iter()
            .any(|attempt| attempt.started && attempt.result.is_some())
    );
}

#[test]
fn brokered_capture_refuses_a_native_credential_mode_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::TrustedUnsafe,
        calls: AtomicUsize::new(0),
    };
    let error = LegacyReviewTaskHost::new(
        &cas,
        SharedEventStore::new(&mut store),
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .err()
    .expect("native transport was admitted as Brokered");
    assert!(error.contains("credential mode"), "{error}");
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

struct UnusableClient;
impl review_broker::ExactBrokerClient for UnusableClient {
    fn handle(&self) -> &review_broker::BrokerHandle {
        panic!("Unsupported operator inspected a Broker Handle")
    }
    fn call(
        &self,
        _: &str,
        _: &[u8],
        _: u64,
    ) -> Result<
        review_broker::BrokerResponse<review_core::BrokerOperationReceiptV2>,
        review_broker::BrokerError,
    > {
        panic!("Unsupported operator invoked a Broker operation")
    }
}

#[test]
fn provider_business_forwarding_preserves_unsupported_capability_refusal() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("store.sqlite")).unwrap();
    let captured = Captured::new(&cas, &mut store);
    let model = Model {
        mode: BrokerCredentialModeV1::Brokered,
        calls: AtomicUsize::new(0),
    };
    let host = LegacyReviewTaskHost::new(
        &cas,
        SharedEventStore::new(&mut store),
        &captured.compiler,
        captured.lease.clone(),
        captured.models(&model),
    )
    .unwrap();
    let models = captured.models(&model);
    let provider = ProviderTaskDomain {
        graph: &captured.graph,
        models: &models,
        inner: &host,
    };
    let generation = captured
        .graph
        .nodes
        .iter()
        .find(|(_, node)| {
            matches!(
                node.operator,
                CompiledOperator::ReviewDomain {
                    operation: ReviewOperation::Generation,
                    ..
                }
            )
        })
        .unwrap()
        .0;
    let input = captured.invocation(generation);
    // RefuseExecution.execute panics: a default implementation must retain Some and refuse.
    let refused =
        plan::RefuseExecution.execute_with_broker(&cas, &input, None, Some(&UnusableClient));
    assert!(
        refused
            .outputs
            .unwrap_err()
            .contains("does not consume Broker Handles")
    );
    assert_eq!(refused.charged_tokens, Some(0));
    // The Provider wrapper must not discard Some on its business-operation forwarding path.
    let forwarded = provider.execute_with_broker(&cas, &input, None, Some(&UnusableClient));
    assert!(
        forwarded
            .outputs
            .unwrap_err()
            .contains("does not consume Broker Handles")
    );
    assert_eq!(forwarded.charged_tokens, Some(0));
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}
