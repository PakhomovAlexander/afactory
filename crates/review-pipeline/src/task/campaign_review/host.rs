//! Connect captured Review operations to the common Task lifecycle. All validation callbacks
//! use CAS and host-owned observations only: the Store calls them while holding its lock.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_core::task::{ArtifactInputV1, TaskResultV1, TaskRevisionV1};
use review_core::{EventType, NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, Producer};
use review_graph::task::{CompiledOperator, ReviewOperation};
use review_graph::{ArtifactMap, Node, NodeKind};
use review_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use review_store::store::task::{TaskLease, task_run_id};
use review_store::{Cas, NewEvent, SharedEventStore};

use super::plan::CampaignReviewPlanCompiler;
use super::{CapturedReviewCompilation, ReviewNodeMapping, artifact_input};
use crate::review_domain::ReviewDomainState;
use crate::task::host::{CapturedTaskAuthority, NoTaskDeveloper, TaskDomain, TaskModelBinding};
use crate::task::{TaskOperatorHost, TaskWorkOutput};
use crate::{DurableReceipt, artifact_ids, port_artifacts};

mod gate_facts;
mod integration;
mod owned;
mod result;
pub use result::RecordedReviewRoundConclusion;
mod worker;

type Ports = BTreeMap<String, ArtifactInputV1>;

pub struct CampaignReviewTaskHost<'store, 'host> {
    domain: ReviewDomainState<'store>,
    compiler: &'host CampaignReviewPlanCompiler,
    captured: CapturedReviewCompilation,
    task: TaskRevisionV1,
    plan: ExecutionPlanV1,
    plan_id: String,
    lease: TaskLease,
    // The same adapter object handles admission and Review for one effective slot binding.
    models: BTreeMap<String, TaskModelBinding<'host>>,
    // Exact durable Task invocation and canonical Review invocation event, by Task node.
    invocations: Mutex<BTreeMap<String, (String, String)>>,
    receipts: Mutex<BTreeMap<String, DurableReceipt>>,
    // Only outputs actually produced by this host or already admitted by the common Store.
    // A CAS envelope and a claimed producer alone cannot manufacture domain authority.
    admitted_outputs: Mutex<BTreeMap<String, Vec<Ports>>>,
    result: Mutex<Option<(String, TaskResultV1)>>,
    owned: Mutex<BTreeMap<String, owned::OwnedReviewChild>>,
}

impl<'store, 'host> CampaignReviewTaskHost<'store, 'host> {
    pub fn new(
        cas: &'store Cas,
        store: SharedEventStore<'store>,
        compiler: &'host CampaignReviewPlanCompiler,
        lease: TaskLease,
        models: BTreeMap<String, TaskModelBinding<'host>>,
    ) -> Result<Self, String> {
        let state = store
            .lock()
            .expect("Task Store")
            .task_projection(cas, lease.task_id())
            .map_err(|e| e.to_string())?
            .ok_or("Unknown Review Task")?;
        let plan_id = state
            .plan_id
            .clone()
            .ok_or("Review Task has no admitted plan")?;
        let plan: ExecutionPlanV1 = serde_json::from_value(
            cas.get_artifact(&plan_id)
                .map_err(|e| e.to_string())?
                .payload,
        )
        .map_err(|e| e.to_string())?;
        let captured = compiler.recompile(cas, &state.revision, &plan)?;
        let round = compiler.round().authority();
        let snapshot: review_core::SourceSnapshot = serde_json::from_value(
            cas.get_json(round.head_snapshot_id())
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let manifest = serde_json::from_value(
            cas.get_json(
                snapshot
                    .artifact_manifest
                    .as_deref()
                    .ok_or("Review Snapshot lacks its manifest")?,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut domain = ReviewDomainState::new(
            cas,
            store.clone(),
            round.run_id.clone(),
            manifest,
            round.subject_kind,
            round.clone(),
        )?;
        domain.configure(&captured.loaded)?;
        domain.checks = captured.loaded.checks().to_vec();
        domain.check_timeout = Duration::from_secs(captured.loaded.check_timeout_seconds());
        let mut admitted_outputs: BTreeMap<String, Vec<Ports>> = BTreeMap::new();
        if let Some(execution) = &state.execution {
            for (id, output) in execution.outputs.values() {
                cas.get_artifact(id).map_err(|e| e.to_string())?;
                admitted_outputs
                    .entry(output.invocation_id.clone())
                    .or_default()
                    .push(output.outputs.clone());
            }
            // A crash may occur after selection and before port publication.
            for node in execution.invocations.keys() {
                if let Some((id, _)) = execution.reusable_output(node) {
                    let output: TaskOutputV1 = serde_json::from_value(
                        cas.get_artifact(&id).map_err(|e| e.to_string())?.payload,
                    )
                    .map_err(|e| e.to_string())?;
                    admitted_outputs
                        .entry(output.invocation_id)
                        .or_default()
                        .push(output.outputs);
                }
            }
        }
        let host = Self {
            domain,
            compiler,
            captured,
            task: state.revision,
            plan,
            plan_id,
            lease,
            models,
            invocations: Mutex::new(BTreeMap::new()),
            receipts: Mutex::new(BTreeMap::new()),
            admitted_outputs: Mutex::new(admitted_outputs),
            result: Mutex::new(None),
            owned: Mutex::new(BTreeMap::new()),
        };
        host.hydrate_owned()?;
        host.validate_transports()?;
        if let Some(execution) = &state.execution {
            host.restore_gate_facts(cas, execution)?;
            for (id, input) in execution.invocations.values() {
                if host.is_integration(input) {
                    let phase = input
                        .inputs
                        .get("phase")
                        .and_then(|p| p.artifact_ids.first())
                        .ok_or("Recorded Integration invocation lacks its phase")?;
                    host.invocations
                        .lock()
                        .expect("Review invocations")
                        .insert(input.node.clone(), (id.clone(), phase.clone()));
                }
            }
        }
        host.hydrate()?;
        Ok(host)
    }

    fn authority(&self) -> CapturedTaskAuthority<'_> {
        CapturedTaskAuthority::for_campaign_review(self.compiler, self, &NoTaskDeveloper)
    }

    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(
                review_sandbox::CacheKind,
            ) -> Result<review_sandbox::CacheSource, review_sandbox::CacheError>
            + Send
            + Sync
            + 'static,
    {
        self.domain.cache_source_resolver = Some(std::sync::Arc::new(resolver));
        self
    }

    pub fn with_container_provider(mut self, provider: review_sandbox::ContainerProvider) -> Self {
        self.domain.container_provider = Some(provider);
        self
    }

    /// Where Warm Workspaces live on this machine. The CLI resolves the XDG cache directory;
    /// tests supply a temporary root. The path never enters a durable record.
    pub fn with_workspace_cache_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.domain.workspace_cache_root = Some(root.into());
        self
    }

    fn providers(&self) -> crate::task::provider::ProviderTaskDomain<'_> {
        crate::task::provider::ProviderTaskDomain {
            graph: &self.captured.compilation.graph,
            models: &self.models,
            inner: self,
        }
    }

    fn is_provider(&self, input: &TaskInvocationV1) -> bool {
        input.plan_id == self.plan_id
            && self
                .captured
                .compilation
                .graph
                .nodes
                .get(&input.node)
                .is_some_and(|node| {
                    matches!(node.operator, CompiledOperator::ProviderAdmission { .. })
                })
    }

    fn operation(
        &self,
        input: &TaskInvocationV1,
    ) -> Result<Option<(Node, ReviewNodeMapping, ReviewOperation)>, String> {
        if input.plan_id != self.plan_id {
            return Err("Review invocation changed its plan".into());
        }
        if self.is_integration(input) {
            return Ok(None);
        }
        if let Some(child) = self
            .owned
            .lock()
            .expect("owned Review mappings")
            .get(&input.node)
        {
            if child.invocation != *input {
                return Err("Owned Review invocation changed registered inputs".into());
            }
            return Ok(Some((
                child.node.clone(),
                child.mapping.clone(),
                child.operation.clone(),
            )));
        }
        let node = self
            .captured
            .compilation
            .graph
            .nodes
            .get(&input.node)
            .ok_or("Unknown Review Task node")?;
        match &node.operator {
            CompiledOperator::ReviewDomain {
                review_node,
                operation,
            } => Ok(Some((
                self.captured.loaded.planned().nodes[review_node].clone(),
                self.captured.compilation.nodes[review_node].clone(),
                operation.clone(),
            ))),
            _ => Ok(None),
        }
    }

    fn raw_inputs(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        mapping: &ReviewNodeMapping,
    ) -> Result<ArtifactMap, String> {
        mapping.restore_inputs(
            cas,
            &mapping
                .inputs
                .keys()
                .map(|name| {
                    (
                        name.clone(),
                        input
                            .inputs
                            .get(name)
                            .map(|p| p.artifact_ids.clone())
                            .unwrap_or_default(),
                    )
                })
                .collect(),
        )
    }

    fn invocation(&self, input: &TaskInvocationV1) -> Result<(String, String), String> {
        let value = self
            .invocations
            .lock()
            .expect("Review invocations")
            .get(&input.node)
            .cloned()
            .ok_or("Review invocation was not published")?;
        let wrapper = self
            .domain
            .cas
            .get_artifact(&value.0)
            .map_err(|e| e.to_string())?;
        if wrapper.artifact_type != review_core::task::execution::TASK_INVOCATION_V1
            || wrapper.payload != serde_json::to_value(input).map_err(|e| e.to_string())?
        {
            return Err("Review invocation differs from its recorded Task inputs".into());
        }
        Ok(value)
    }

    fn hydrate(&self) -> Result<(), String> {
        let events = self
            .domain
            .store
            .lock()
            .expect("Task Store")
            .replay(&self.domain.run_id)
            .map_err(|e| e.to_string())?;
        for event in events {
            if event.causation_id.as_deref() != Some(&self.domain.authority.round_event_id) {
                continue;
            }
            let Some(node) = event.node_id.as_ref() else {
                continue;
            };
            match event.event_type {
                EventType::NodeOutputReceiptV1 => {
                    let payload: NodeOutputReceiptPayloadV1 =
                        serde_json::from_value(event.payload).map_err(|e| e.to_string())?;
                    self.receipts.lock().expect("Review receipts").insert(
                        node.clone(),
                        DurableReceipt {
                            payload,
                            attempt_id: event.attempt_id,
                        },
                    );
                }
                EventType::GateDecisionV1 => {
                    self.domain.gates.lock().expect("Review Gates").insert(
                        node.clone(),
                        serde_json::from_value(event.payload).map_err(|e| e.to_string())?,
                    );
                }
                EventType::GateExecutionBoundV1 => {
                    self.domain
                        .execution_bindings
                        .lock()
                        .expect("Review execution bindings")
                        .insert(
                            node.clone(),
                            serde_json::from_value(event.payload).map_err(|e| e.to_string())?,
                        );
                }
                EventType::CacheSnapshotMaterializedV1 => {
                    let value: review_core::RunCacheSnapshotV5 =
                        serde_json::from_value(event.payload).map_err(|e| e.to_string())?;
                    self.domain
                        .cache_snapshots
                        .lock()
                        .expect("Review caches")
                        .insert((node.clone(), value.kind), value);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn current(&self, attempt: &PreparedTaskAttempt) -> Result<Instant, String> {
        self.domain
            .store
            .lock()
            .expect("Task Store")
            .check_task_attempt_current(self.domain.cas, &self.lease, attempt, &self.authority())
            .map_err(|e| e.to_string())?;
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis(),
        )
        .map_err(|e| e.to_string())?;
        let remaining = attempt
            .reservation()
            .deadline_unix_ms
            .checked_sub(now)
            .filter(|n| *n > 0)
            .ok_or("Review Attempt deadline expired")?;
        Instant::now()
            .checked_add(Duration::from_millis(remaining))
            .ok_or_else(|| "Review Attempt deadline overflow".into())
    }

    fn producer(
        &self,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
    ) -> Result<Producer, String> {
        let invocation = self.invocation(input)?.0;
        Ok(if let Some(attempt) = attempt {
            Producer::Attempt {
                run_id: task_run_id(&self.task.task_id).map_err(|e| e.to_string())?,
                node_id: input.node.clone(),
                attempt_id: attempt.id().into(),
            }
        } else {
            Producer::KernelOperation {
                run_id: self.domain.run_id.clone(),
                node_id: Some(input.node.clone()),
                operation_id: format!("task-review-output@1:{invocation}"),
            }
        })
    }

    fn lift(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        mapping: &ReviewNodeMapping,
        raw: &ArtifactMap,
        producer: &Producer,
    ) -> Result<Ports, String> {
        mapping
            .outputs
            .iter()
            .filter(|(_, port)| {
                raw.get(&port.review_port)
                    .is_some_and(|ids| !ids.is_empty())
            })
            .map(|(name, port)| {
                let ids = raw[&port.review_port]
                    .iter()
                    .map(|id| {
                        port.codec.capture(
                            cas,
                            id,
                            producer.clone(),
                            Some(self.domain.authority.head_snapshot_id.clone()),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let contract = self.output_contract(input, name)?;
                Ok((
                    name.clone(),
                    artifact_input(cas, &contract.artifact_type, ids, contract.cardinality)?,
                ))
            })
            .collect()
    }

    fn flush_facts(&self, node: &str) -> Result<(), String> {
        let mut pending = self.domain.reviewer_events.lock().expect("Review events");
        let events: Vec<_> = pending
            .iter()
            .filter(|((id, _), _)| id == node)
            .map(|(_, event)| event.clone())
            .collect();
        if !events.is_empty() {
            self.domain.append_batch(&events)?;
        }
        pending.retain(|((id, _), _)| id != node);
        Ok(())
    }

    fn execute_operation(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Ports, String> {
        let (node, mapping, operation) = self.operation(input)?.ok_or("Not a Review operation")?;
        let raw = self.raw_inputs(cas, input, &mapping)?;
        let mut companions = BTreeMap::new();
        let raw_outputs = match operation {
            ReviewOperation::Generation => self.domain.run_generation(&node)?,
            ReviewOperation::Gate => {
                let deadline =
                    self.current(attempt.ok_or("Review Gate has no started Attempt")?)?;
                let result = self.domain.run_gate_controlled(
                    &node.id,
                    Some(deadline),
                    cancellation,
                    attempt.map(PreparedTaskAttempt::id),
                );
                if result.is_err() {
                    self.domain.record_unmaterialized_cache_failures(
                        &node.id,
                        review_core::RunCacheFailureReasonV5::GateSetupFailed,
                    );
                }
                // Facts of executed checks survive a crash before Task settlement/publication.
                // They grant no Task output selection or acceptance by themselves.
                self.flush_facts(&node.id)?;
                BTreeMap::from([(node.outputs[0].name.clone(), result?)])
            }
            ReviewOperation::Gather => BTreeMap::from([(
                node.outputs[0].name.clone(),
                self.domain.run_gather(&node, &raw)?,
            )]),
            ReviewOperation::Ledger => {
                let reduced = self.domain.reduce_ledger(&node, &raw)?;
                companions = reduced.canonical;
                reduced.original
            }
            ReviewOperation::Slicer => {
                BTreeMap::from([(node.outputs[0].name.clone(), self.domain.run_slicer(&node)?)])
            }
            ReviewOperation::Reviewer { .. } | ReviewOperation::Scatter { .. } => {
                return Err("Review Worker requires its started common Attempt".into());
            }
        };
        let producer = self.producer(input, attempt)?;
        let mut outputs = self.lift(cas, input, &mapping, &raw_outputs, &producer)?;
        for (port, id) in companions {
            let contract = &self.captured.compilation.graph.nodes[&input.node]
                .contract
                .outputs[&port];
            outputs.insert(
                port,
                artifact_input(cas, &contract.artifact_type, vec![id], contract.cardinality)?,
            );
        }
        if matches!(operation, ReviewOperation::Gate) {
            self.gate_outcome(cas, &node, &raw_outputs, &mut outputs, producer)?;
        }
        Ok(outputs)
    }

    fn gate_outcome(
        &self,
        cas: &Cas,
        node: &Node,
        raw: &ArtifactMap,
        outputs: &mut Ports,
        producer: Producer,
    ) -> Result<(), String> {
        use review_core::task::campaign_review::{
            CAMPAIGN_REVIEW_GATE_OUTCOME_V1, CampaignReviewGateOutcomeV1,
        };
        use review_core::task::pipeline::ReceiptOutcomeV1;
        let decision_id = &raw[&node.outputs[0].name][0];
        let decision: review_check::GateDecision =
            serde_json::from_value(cas.get_json(decision_id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let value = CampaignReviewGateOutcomeV1 {
            round_event_id: self.domain.authority.round_event_id.clone(),
            review_node: node.id.clone(),
            gate_decision_id: decision_id.clone(),
            outcome: if decision.passed() {
                ReceiptOutcomeV1::Passed
            } else {
                ReceiptOutcomeV1::Failed
            },
        };
        value.validate()?;
        let id = cas
            .put_artifact(
                CAMPAIGN_REVIEW_GATE_OUTCOME_V1,
                producer,
                vec![decision_id.clone()],
                None,
                serde_json::to_value(value).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
            .0;
        outputs.insert(
            "outcome".into(),
            artifact_input(
                cas,
                CAMPAIGN_REVIEW_GATE_OUTCOME_V1,
                vec![id],
                review_core::PortCardinality::One,
            )?,
        );
        Ok(())
    }
}

impl CampaignReviewTaskHost<'_, '_> {
    /// The context bytes for this invocation, rendered identically by the capture entry point
    /// and by the admission recheck. A reviewer's Worker context needs the reserved Attempt;
    /// every pure Review operation is its own durable invocation identity.
    fn render_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        if let Some(ReviewOperation::Reviewer { .. }) = self
            .operation(input)?
            .map(|(_, _, op)| op)
            .filter(|_| !self.is_provider(input))
        {
            return self.worker_context(cas, input, attempt);
        }
        self.unreserved_context(input, attempt.feedback_ids())
    }

    /// The context bytes for an operation that carries no Attempt authority: every pure Review
    /// operation, whose identity is its durable invocation, and the Provider capability probe.
    pub fn unreserved_context(
        &self,
        input: &TaskInvocationV1,
        feedback: &[String],
    ) -> Result<String, String> {
        if self.is_provider(input) {
            return self
                .providers()
                .render_probe_context(self.domain.cas, input, feedback);
        }
        if self.operation(input)?.is_some_and(|(_, _, op)| {
            matches!(
                op,
                ReviewOperation::Reviewer { .. } | ReviewOperation::Scatter { .. }
            )
        }) {
            return Err("Review Worker context requires its actual reserved Attempt".into());
        }
        if !feedback.is_empty() {
            return Err("Pure Review operation cannot consume retry feedback".into());
        }
        Ok(self.invocation(input)?.0)
    }
}

impl TaskOperatorHost for CampaignReviewTaskHost<'_, '_> {
    fn prepare_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
    ) -> Result<crate::task::TaskOwnedChildrenInputs, String> {
        self.prepare_owned_review(cas, parent)
    }

    fn complete_owned_children(
        &self,
        cas: &Cas,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
    ) -> Result<Ports, String> {
        self.complete_owned_review(cas, parent, children, facts)
    }

    fn commit_domain_invocation(
        &self,
        cas: &Cas,
        invocation_id: &str,
        input: &TaskInvocationV1,
    ) -> Result<(), String> {
        if self.is_integration(input) {
            return self.commit_integration_invocation(cas, invocation_id, input);
        }
        self.hydrate_owned_invocation(cas, input)?;
        let Some((node, mapping, _)) = self.operation(input)? else {
            return Ok(());
        };
        let raw = self.raw_inputs(cas, input, &mapping)?;
        let payload = NodeInvocationPayloadV1 {
            node: node.id.clone(),
            inputs: port_artifacts(&node.inputs, &raw, &self.domain.authority.head_snapshot_id),
        };
        let payload = serde_json::to_value(payload).map_err(|e| e.to_string())?;
        let mut store = self.domain.store.lock().expect("Task Store");
        let state = store
            .task_projection(cas, &self.task.task_id)
            .map_err(|e| e.to_string())?
            .ok_or("Review Task disappeared before invocation publication")?;
        if state.has_recording_recovery() {
            store
                .check_task_recorded_invocation(cas, &self.lease, invocation_id, &self.authority())
                .map_err(|e| e.to_string())?;
        } else if !self.captured.compilation.graph.owned_children.is_empty() {
            store
                .check_current_task_plan_for_recording(cas, &self.lease, &self.authority())
                .map_err(|e| e.to_string())?;
        } else {
            store
                .check_task_dispatch(cas, &self.lease, &self.authority())
                .map_err(|e| e.to_string())?;
        }
        if state
            .execution
            .as_ref()
            .and_then(|execution| execution.invocations.get(&input.node))
            != Some(&(invocation_id.to_owned(), input.clone()))
        {
            return Err("Review invocation lacks its exact common Store publication".into());
        }
        let recorded = store
            .replay(&self.domain.run_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|event| {
                event.event_type == EventType::NodeInvocationV1
                    && event.node_id.as_deref() == Some(&node.id)
                    && event.causation_id.as_deref() == Some(&self.domain.authority.round_event_id)
            });
        let event = if let Some(recorded) = recorded {
            if recorded.payload != payload {
                return Err("Review invocation changed recorded inputs".into());
            }
            recorded
        } else {
            store
                .append(
                    &self.domain.run_id,
                    cas,
                    self.domain.bind_authority(
                        NewEvent::new(EventType::NodeInvocationV1, payload)
                            .node(&node.id)
                            .referencing(artifact_ids(&raw)),
                    ),
                )
                .map_err(|e| e.to_string())?
        };
        drop(store);
        self.invocations
            .lock()
            .expect("Review invocations")
            .insert(input.node.clone(), (invocation_id.into(), event.event_id));
        if node.kind == NodeKind::Reviewer {
            self.domain
                .reviewer_input_artifacts
                .lock()
                .expect("Review inputs")
                .insert(node.id.clone(), artifact_ids(&raw));
            // Recorded before the common runtime reserves the node's first Attempt, so every
            // Attempt of the Round starts from the same declared Warm Set.
            // The Task-hosted frontend installs no session capability, so no delta is sized.
            self.domain.select_warm_set(&node.id, None)?;
        }
        Ok(())
    }

    fn commit_domain_output(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        output_id: &str,
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        let Some((node, mapping, operation)) = self.operation(input)? else {
            return Ok(());
        };
        if matches!(operation, ReviewOperation::Reviewer { .. }) {
            self.publish_reviewer(cas, input, output_id, output)?;
        }
        if self
            .captured
            .compilation
            .graph
            .owned_children
            .contains_key(&input.node)
        {
            self.publish_owned_shards(cas, input, output_id)?;
        }
        let raw = mapping
            .outputs
            .iter()
            .map(|(name, port)| {
                let ids = output
                    .outputs
                    .get(name)
                    .map(|p| p.artifact_ids.as_slice())
                    .unwrap_or_default();
                Ok((
                    port.review_port.clone(),
                    ids.iter()
                        .map(|id| port.codec.restore(cas, id))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            })
            .collect::<Result<ArtifactMap, String>>()?;
        let recorded = self
            .receipts
            .lock()
            .expect("Review receipts")
            .get(&node.id)
            .cloned();
        self.domain
            .publish_outputs(&node, &raw, recorded.as_ref())?;
        // Hydrate the just-published receipt for acknowledgement loss in this process too.
        self.receipts.lock().expect("Review receipts").insert(
            node.id.clone(),
            DurableReceipt {
                payload: NodeOutputReceiptPayloadV1 {
                    node: node.id.clone(),
                    outputs: port_artifacts(
                        &node.outputs,
                        &raw,
                        &self.domain.authority.head_snapshot_id,
                    ),
                },
                attempt_id: recorded.and_then(|r| r.attempt_id),
            },
        );
        Ok(())
    }

    fn prepare_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        attempt: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        self.render_context(cas, input, attempt)
    }

    fn execute(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        definition: &review_graph::task::CompiledNode,
        attempt: Option<&PreparedTaskAttempt>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        if let Err(error) = crate::task::control::check(cancellation) {
            return crate::task::control::refused(error);
        }

        if self.is_provider(input) {
            return self
                .providers()
                .execute(cas, input, definition, attempt, cancellation);
        }
        if self.is_integration(input) {
            return self.execute_integration(cas, input, attempt, cancellation);
        }
        let reviewer = self
            .operation(input)
            .ok()
            .flatten()
            .is_some_and(|(_, _, op)| matches!(op, ReviewOperation::Reviewer { .. }));
        let mut result = if reviewer {
            self.execute_reviewer(cas, input, attempt, cancellation)
        } else {
            TaskWorkOutput {
                usage_observation: None,
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                outputs: self.execute_operation(cas, input, attempt, cancellation),
                charged_tokens: Some(0),
                raw_artifact_ids: vec![],
                usage_id: None,
                feedback_id: None,
            }
        };
        if self
            .operation(input)
            .ok()
            .flatten()
            .is_some_and(|(_, _, op)| matches!(op, ReviewOperation::Gate))
            && let Some(attempt) = attempt
        {
            match self.capture_gate_facts(cas, input, attempt) {
                Ok(ids) => result.raw_artifact_ids.extend(ids),
                Err(error) => {
                    result.outputs = Err(format!(
                        "Cannot retain Review Gate facts: {error}; operation: {:?}",
                        result.outputs.err()
                    ))
                }
            }
        }
        if let Ok(outputs) = &result.outputs {
            if let Ok((invocation, _)) = self.invocation(input) {
                self.admitted_outputs
                    .lock()
                    .expect("Review outputs")
                    .entry(invocation)
                    .or_default()
                    .push(outputs.clone());
            }
        }
        result
    }
}

impl TaskDomain for CampaignReviewTaskHost<'_, '_> {
    fn validate_review_integration_selection(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        self.validate_integration_selection(cas, task, plan, phase, evidence)
    }
    fn validate_review_integration_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        phase: &review_core::task::review_integration::TaskReviewIntegrationPhaseV1,
        _report: &review_core::task::report::TaskRunReportV1,
        _events: &[NewEvent],
        evidence: &review_store::store::task::review_integration::TaskReviewIntegrationEvidence,
    ) -> Result<(), String> {
        // Store additionally requires the exact selected common output and runs the original
        // complete canonical checks/attestation/commit validator in its atomic transaction.
        self.validate_integration_selection(cas, task, plan, phase, evidence)
    }

    fn validate_owned_children(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
    ) -> Result<(), String> {
        if task != &self.task || plan != &self.plan {
            return Err("Owned Review changed captured Task authority".into());
        }
        self.check_owned_set(cas, parent, children)
    }

    fn validate_owned_completion(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        parent: &TaskInvocationV1,
        children: &review_core::task::owned_children::TaskOwnedChildSetV1,
        facts: &[review_store::store::task::execution::owned::TaskOwnedChildEvidence],
        output: &TaskOutputV1,
    ) -> Result<(), String> {
        if task != &self.task || plan != &self.plan {
            return Err("Owned Review changed captured Task authority".into());
        }
        self.check_owned_completion(cas, parent, children, facts, output)
    }

    fn assemble_result(
        &self,
        cas: &Cas,
        _: &review_store::store::task::TaskProjection,
        _: &review_graph::RunReport,
    ) -> Result<TaskResultV1, String> {
        self.assemble_recorded_result(cas)
    }
    fn validate_context(
        &self,
        cas: &Cas,
        input: &TaskInvocationV1,
        attempt: &ReservedTaskAttempt,
        context_id: &str,
    ) -> Result<(), String> {
        let artifact = cas.get_artifact(context_id).map_err(|e| e.to_string())?;
        for id in &artifact.input_artifacts {
            cas.verify(id).map_err(|e| e.to_string())?;
        }
        if self.render_context(cas, input, attempt)? != context_id {
            return Err("Review context changed the reserved Attempt or rendered inputs".into());
        }
        Ok(())
    }

    fn validate_output(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        plan: &ExecutionPlanV1,
        input: &TaskInvocationV1,
        output: &TaskOutputV1,
        definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        if task != &self.task || plan != &self.plan {
            return Err("Review output changed captured Task authority".into());
        }
        if self.is_provider(input) {
            return self
                .providers()
                .validate_output(cas, task, plan, input, output, definition);
        }
        if !self.is_integration(input) && self.operation(input)?.is_none() {
            return Ok(());
        }
        if self.invocation(input)?.0 != output.invocation_id {
            return Err("Review output belongs to another invocation".into());
        }
        if !self
            .admitted_outputs
            .lock()
            .expect("Review outputs")
            .get(&output.invocation_id)
            .is_some_and(|values| values.contains(&output.outputs))
        {
            return Err(
                "Review output was neither produced nor durably admitted by the host".into(),
            );
        }
        for port in output.outputs.values() {
            artifact_input(
                cas,
                &port.artifact_type,
                port.artifact_ids.clone(),
                port.cardinality,
            )?;
        }
        Ok(())
    }

    fn validate_result(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        result: &TaskResultV1,
    ) -> Result<(), String> {
        let expected = self.result.lock().expect("Review result");
        let (report_id, expected) = expected
            .as_ref()
            .ok_or("Canonical Review Task conclusion has not been assembled")?;
        if task != &self.task || result != expected {
            return Err("Task result differs from its canonical Review conclusion".into());
        }
        cas.get_artifact(report_id).map_err(|e| e.to_string())?;
        for id in &result.evidence {
            let artifact = cas.get_artifact(id).map_err(|e| e.to_string())?;
            for reference in artifact.input_artifacts {
                cas.verify(&reference).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    fn validate_review_continuation(
        &self,
        cas: &Cas,
        previous: &TaskRevisionV1,
        next: &TaskRevisionV1,
        previous_plan: &ExecutionPlanV1,
        next_plan: &ExecutionPlanV1,
        handoff: &review_core::task::review_handoff::TaskReviewHandoffV1,
    ) -> Result<(), String> {
        // CapturedTaskAuthority has recompiled the successor with its successor compiler.
        // This still-live predecessor host supplies the old captured Round authority without
        // reopening or locking Store inside its validation callback.
        if previous != &self.task
            || previous_plan != &self.plan
            || handoff.predecessor_plan_id != self.plan_id
            || handoff.predecessor_revision_id != previous_plan.task_revision_id
            || handoff.successor_revision_id != next_plan.task_revision_id
            || next.task_id != previous.task_id
            || next.authority != previous.authority
            || next.limits != previous.limits
            || next.provenance != previous.provenance
        {
            return Err("Review handoff changed its captured predecessor or Task authority".into());
        }
        if matches!(
            handoff.evidence,
            review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::ClosedRound { .. }
                | review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::IntegratedRound { .. }
        ) && self.compiler.mode()? != review_config::captured_review::ReviewMode::Heavy
        {
            return Err(
                "Only a captured heavy Campaign may continue to another numeric Round".into(),
            );
        }
        if let review_core::task::review_handoff::TaskReviewHandoffEvidenceV1::IntegratedRound {
            phase_id,
            ..
        } = &handoff.evidence
        {
            let phase =
                review_store::store::task::review_integration::read_task_review_integration(
                    cas, phase_id,
                )
                .map_err(|e| e.to_string())?;
            let review_core::task::review_integration::TaskReviewIntegrationSelectionV1::Prepared {
                derived_snapshot_id,
                ..
            } = phase.selection
            else {
                return Err("Integrated continuation lacks a prepared Snapshot".into());
            };
            let round: review_core::task::campaign_review::CampaignReviewRoundV1 =
                serde_json::from_value(
                    cas.get_artifact(&handoff.successor_round_id)
                        .map_err(|e| e.to_string())?
                        .payload,
                )
                .map_err(|e| e.to_string())?;
            if phase.plan_id != self.plan_id || round.head_snapshot_id != derived_snapshot_id {
                return Err("Integrated continuation changed the exact prepared head".into());
            }
        }
        self.compiler.recompile(cas, previous, previous_plan)?;
        Ok(())
    }
}
