//! The Kernel: what a pipeline needs to run one generation, and the run loop the scheduler
//! drives through [`Dispatch`]. Construction, budgets, the generation-local Ledger cache, event
//! publication, sandboxes, and the terminal RunReport live here; each node kind's execution is
//! a child module with its own `impl Kernel` block.

mod attempt;
mod gate;
mod integration;
mod ledger;
mod proposal;
mod replay;
mod reviewer;
mod scatter;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_attempt::{AttemptId, AttemptLedger, Budget, BudgetLedger, BudgetScope, Reservation};
use review_broker::{AuthorityError, BrokerHandle, LeaseAuthority, ReceiptError, ReceiptSink};
use review_check::{CheckDefinition, Command, GateDecision};
use review_core::event::{AttemptAdmittedPayloadV1, AttemptFencedPayloadV1};
use review_core::{
    BrokerCredentialModeV1, BrokerLeaseV1, BrokerOperationReceiptV1, EventType,
    NodeInvocationPayloadV1, NodeOutputReceiptPayloadV1, ReviewerExecutionBindingV1,
    RunCacheFailureReasonV5, RunCacheFailureV5, RunCacheKindV5, RunCacheSnapshotV5,
    RunExecutionBindingV4, RunNodeOutcomeV2, RunNodeReportV2, RunReportPayloadV3,
    RunReportPayloadV4, RunReportPayloadV5, run_report_closes_round,
};
use review_graph::{
    ArtifactMap, Dispatch, Node, NodeFailureClass, NodeKind, NodeOutcome, RunReport,
};
use review_runner::{ContextManifest, ReviewerAdapter, TokenUsage};
use review_sandbox::{CacheError, CacheKind, CacheSource, ContainerProvider, Mode, Sandbox};
use review_source_git::Manifest;
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ledger, LedgerProjection, NewEvent, StoreError,
};

use crate::authority::RoundAuthority;
use crate::kernel::replay::{DurableReceipt, SelectedReviewer, replay_execution};
use crate::verdict::{RunVerdict, persisted_verdict, run_suppression_reason, run_verdict};
use crate::{
    BrokerProvider, artifact_ids, bind_single_output, is_change_set_port,
    is_generation_finding_set_output, is_generation_prior_findings_output,
    is_reviewer_prior_set_input, port_artifacts, run_cache_kind,
};

type CacheSourceResolver = dyn Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync;

/// The run's own budget accounts, alongside the caps that opened them.
struct Budgets {
    attempt_cap: u64,
    ledger: Mutex<BudgetLedger>,
}

pub(crate) struct PreparedReviewerAttempt {
    attempt: AttemptId,
    reservation: Option<Reservation>,
    refusal_history_id: Option<String>,
}

/// Selected Attempt accounting reconstructed from its durable provenance artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptEvidence {
    pub node: String,
    pub attempt_id: String,
    pub cost_tokens: u64,
    pub usage: TokenUsage,
    pub context_manifest: ContextManifest,
    pub raw_artifact: String,
    pub result_artifact: String,
}

/// What a pipeline needs to run one generation.
pub struct Kernel<'a> {
    cas: &'a Cas,
    store: Mutex<&'a mut EventStore>,
    run_id: String,
    /// The immutable subject. Every node is materialized from this, so they all inspect the
    /// same content by construction rather than by discipline.
    snapshot: Manifest,
    subject: review_core::SubjectKind,
    pipeline_version: u32,
    authority: RoundAuthority,
    checks: Vec<CheckDefinition>,
    check_timeout: Duration,
    /// Absent only for frozen v1/v2 pipeline semantics. V3 resolves this exact Gate binding from
    /// captured authority before any candidate check executes.
    gate_execution: Option<review_config::GateExecutionSpec>,
    integration: Option<review_config::IntegrationSpec>,
    /// Optional machine-resolved provider. The CLI normally lets the kernel probe locally;
    /// embedding callers and deterministic boundary tests may bind an already-probed provider.
    container_provider: Option<ContainerProvider>,
    /// Machine-local sources resolved by the CLI. Host paths never enter captured pipeline
    /// authority or durable events.
    cache_sources: BTreeMap<CacheKind, CacheSource>,
    cache_source_resolver: Option<Arc<CacheSourceResolver>>,
    execution_bindings: Mutex<BTreeMap<String, RunExecutionBindingV4>>,
    cache_snapshots: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheSnapshotV5>>,
    cache_failures: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheFailureV5>>,
    reviewers: BTreeMap<String, Box<dyn ReviewerAdapter>>,
    reviewer_execution: BTreeMap<String, review_config::ReviewerExecutionSpec>,
    broker_providers: BTreeMap<String, BrokerProvider>,
    demand_requirements: BTreeMap<String, review_core::DemandRequirement>,
    slicing: BTreeMap<String, crate::scatter::StaticSlicePolicy>,
    closeouts: BTreeMap<String, String>,
    static_node_ids: BTreeSet<String>,
    /// Dynamic reviewer ID -> owning static Scatter. The binding is installed only after the
    /// complete SliceSet is durable and lets existing reviewer authority remain keyed by the
    /// captured static graph.
    dynamic_reviewer_bases: Mutex<BTreeMap<String, String>>,
    fan_out_cap: Option<u64>,
    /// Worker nodes with their own Attempt cap (`[[nodes]] budget.attempt`); every other node
    /// reserves the pipeline-wide attempt cap.
    node_attempt_caps: BTreeMap<String, u64>,
    attempts: Mutex<AttemptLedger>,
    budgets: Option<Budgets>,
    /// Retries per node, spent on timeouts or an inadmissible returned result. A retry is a new
    /// attempt: it fences its predecessor and reserves its own budget.
    timeout_retries: u32,
    /// Gate decisions by gate node. Keyed, so two gates in one pipeline never share a verdict.
    gates: Mutex<BTreeMap<String, GateDecision>>,
    /// The campaign's prior findings, as a CAS artifact every reviewer attempt receives —
    /// labelled data resolved by the kernel, which is what makes round N+1 a re-examination
    /// of round N's claims instead of a fresh look that happens to share a repository.
    prior_findings: Option<String>,
    /// Reviewer result and gate events held until their node receipt can publish them as one
    /// batch. Each `(node, seq)` preserves emission order inside that node. Dispatch and terminal
    /// failure events are deliberately not buffered: dispatch must be durable before external
    /// execution, and a failed attempt must be durable before its retry dispatch. Their ordering
    /// across concurrently executing nodes therefore records real completion order rather than
    /// claiming whole-log determinism that the scheduler cannot provide.
    reviewer_events: Mutex<Vec<((String, u64), NewEvent)>>,
    reviewer_event_seq: Mutex<u64>,
    /// First attempts are reserved, assigned, and durably dispatched by the scheduler thread in
    /// plan order before any external model call starts. The worker removes its prepared entry.
    prepared_attempts: Mutex<BTreeMap<String, PreparedReviewerAttempt>>,
    failure_classes: Mutex<BTreeMap<String, NodeFailureClass>>,
    /// The snapshot materialized once, cloned per sandbox. Built lazily on the first sandbox
    /// request — the gate's — so a run that never reaches a sandbox never pays for it.
    template: Mutex<Option<std::sync::Arc<review_sandbox::SandboxTemplate>>>,
    /// One kernel generation has exactly one durable conclusion.
    report_published: Mutex<bool>,
    /// Latest projection of this generation's durable log. Every append advances its watermark,
    /// including events that leave the visible Ledger unchanged; an out-of-order concurrent
    /// observation drops the cache so the next reader rebuilds. Gather installs its live ingest.
    ledger_cache: Mutex<Option<LedgerProjection>>,
    replayed_invocations: BTreeMap<String, NodeInvocationPayloadV1>,
    replayed_outputs: BTreeMap<String, DurableReceipt>,
    replayed_refusal_histories: BTreeMap<String, Vec<String>>,
    reviewer_selections: Mutex<BTreeMap<String, SelectedReviewer>>,
    reviewer_input_artifacts: Mutex<BTreeMap<String, Vec<String>>>,
    /// Validated graph provenance: downstream node -> input port -> exact upstream node, output
    /// port, and kind. The kind distinguishes reviewer selection from deterministic node output.
    input_bindings: review_config::InputBindings,
    /// Outputs made durable in this scheduler run, keyed by their producing node. Downstream
    /// canonical reducers consult only this map plus the validated graph when binding artifacts.
    node_outputs: Mutex<BTreeMap<String, ArtifactMap>>,
    replayed_spent: u64,
    replayed_fan_out_spent: BTreeMap<String, u64>,
    replayed_node_spent: BTreeMap<String, u64>,
}

struct KernelBrokerBoundary<'kernel, 'store> {
    kernel: &'kernel Kernel<'store>,
}

impl LeaseAuthority for KernelBrokerBoundary<'_, '_> {
    fn ensure_current(
        &self,
        lease: &BrokerLeaseV1,
        handle: &BrokerHandle,
    ) -> Result<(), AuthorityError> {
        if lease.campaign_id != self.kernel.run_id
            || lease.round_event_id != self.kernel.authority.round_event_id
            || lease.node_id.trim().is_empty()
        {
            return Err(AuthorityError);
        }
        let events = self
            .kernel
            .store
            .lock()
            .expect("event store")
            .replay(&self.kernel.run_id)
            .map_err(|_| AuthorityError)?;
        let latest_round = events
            .iter()
            .rev()
            .find(|event| event.event_type == EventType::RoundStartedV1)
            .map(|event| event.event_id.as_str());
        if latest_round != Some(lease.round_event_id.as_str()) {
            return Err(AuthorityError);
        }
        let mut latest_attempt = None;
        let mut bound = false;
        let mut terminal = false;
        for event in events.iter().filter(|event| {
            event.causation_id.as_deref() == Some(lease.round_event_id.as_str())
                && event.node_id.as_deref() == Some(lease.node_id.as_str())
        }) {
            if event.event_type == EventType::AttemptDispatchedV1 {
                latest_attempt = event.attempt_id.as_deref();
            }
            if event.attempt_id.as_deref() != Some(lease.attempt_id.as_str()) {
                continue;
            }
            if event.event_type == EventType::ReviewerExecutionBoundV1 {
                let binding: ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload.clone()).map_err(|_| AuthorityError)?;
                bound = binding.admitted
                    && binding.lease_epoch == lease.lease_epoch
                    && binding.broker_handle.as_deref() == Some(handle.as_str());
            }
            if matches!(
                event.event_type,
                EventType::AttemptAdmittedV1
                    | EventType::AttemptFailedV1
                    | EventType::AttemptFencedV1
                    | EventType::AttemptReleasedV1
            ) {
                terminal = true;
            }
        }
        (latest_attempt == Some(lease.attempt_id.as_str()) && bound && !terminal)
            .then_some(())
            .ok_or(AuthorityError)
    }
}

impl ReceiptSink for KernelBrokerBoundary<'_, '_> {
    fn record(&self, receipt: &BrokerOperationReceiptV1) -> Result<(), ReceiptError> {
        let event = NewEvent::new(
            EventType::BrokerOperationCompletedV1,
            serde_json::to_value(receipt).map_err(|_| ReceiptError::Unavailable)?,
        )
        .node(&receipt.node)
        .attempt(&receipt.attempt_id)
        .correlating(&receipt.handle_id);
        let event = self.kernel.bind_authority(event);
        let appended = self.kernel.store.lock().expect("event store").append(
            &self.kernel.run_id,
            self.kernel.cas,
            event,
        );
        match appended {
            Ok(event) => {
                self.kernel
                    .fold_appended_into_ledger_cache(std::slice::from_ref(&event));
                Ok(())
            }
            Err(StoreError::AttemptNotCurrent) => Err(ReceiptError::AuthorityRevoked),
            Err(_) => Err(ReceiptError::Unavailable),
        }
    }
}

impl<'a> Kernel<'a> {
    fn new(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        pipeline_version: u32,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        let run_id = run_id.into();
        if authority.run_id != run_id {
            return Err("Round authority belongs to a different Campaign run".into());
        }
        if snapshot.content_digest() != authority.head_content_digest {
            return Err("executed manifest does not match the Round Subject Snapshot".into());
        }
        if subject != authority.subject_kind {
            return Err("pipeline Subject kind disagrees with Round authority".into());
        }
        let replayed = replay_execution(store, cas, &run_id, &authority)?;
        if !replayed.outstanding_attempts.is_empty() {
            let events: Vec<NewEvent> = replayed
                .outstanding_attempts
                .iter()
                .map(|(node, attempt, charged)| {
                    let mut event = NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::to_value(AttemptFencedPayloadV1 {
                            reason: "process ended before attempt publication".into(),
                            charged: Some(*charged),
                        })
                        .expect("typed attempt fence"),
                    )
                    .node(node)
                    .attempt(attempt)
                    .caused_by(authority.round_event_id.clone())
                    .correlating(authority.subject_id.clone());
                    event.artifact_refs.extend(authority.artifact_refs());
                    event
                })
                .collect();
            store
                .append_batch(&run_id, cas, &events)
                .map_err(|error| error.to_string())?;
        }
        let attempts =
            AttemptLedger::scoped(&authority.round_event_id, replayed.attempt_counts.clone());
        let prior_findings = Some(authority.prior_finding_set_id.clone());
        let reviewer_input_artifacts = replayed
            .invocations
            .iter()
            .map(|(node, invocation)| {
                (
                    node.clone(),
                    invocation
                        .inputs
                        .iter()
                        .flat_map(|port| port.artifact_ids.iter().cloned())
                        .collect(),
                )
            })
            .collect();
        Ok(Kernel {
            cas,
            store: Mutex::new(store),
            run_id,
            snapshot,
            subject,
            pipeline_version,
            authority,
            checks: Vec::new(),
            check_timeout: Duration::from_secs(3600),
            gate_execution: None,
            integration: None,
            container_provider: None,
            cache_sources: BTreeMap::new(),
            cache_source_resolver: None,
            execution_bindings: Mutex::new(replayed.execution_bindings),
            cache_snapshots: Mutex::new(replayed.cache_snapshots),
            cache_failures: Mutex::new(BTreeMap::new()),
            reviewers: BTreeMap::new(),
            reviewer_execution: BTreeMap::new(),
            broker_providers: BTreeMap::new(),
            demand_requirements: BTreeMap::new(),
            slicing: BTreeMap::new(),
            closeouts: BTreeMap::new(),
            static_node_ids: BTreeSet::new(),
            dynamic_reviewer_bases: Mutex::new(BTreeMap::new()),
            fan_out_cap: None,
            node_attempt_caps: BTreeMap::new(),
            attempts: Mutex::new(attempts),
            budgets: None,
            timeout_retries: 1,
            gates: Mutex::new(replayed.gates),
            prior_findings,
            reviewer_events: Mutex::new(Vec::new()),
            reviewer_event_seq: Mutex::new(0),
            prepared_attempts: Mutex::new(BTreeMap::new()),
            failure_classes: Mutex::new(BTreeMap::new()),
            template: Mutex::new(None),
            report_published: Mutex::new(false),
            ledger_cache: Mutex::new(None),
            replayed_invocations: replayed.invocations,
            replayed_outputs: replayed.outputs,
            replayed_refusal_histories: replayed.refusal_histories,
            reviewer_selections: Mutex::new(replayed.selected_reviewers),
            reviewer_input_artifacts: Mutex::new(reviewer_input_artifacts),
            input_bindings: BTreeMap::new(),
            node_outputs: Mutex::new(BTreeMap::new()),
            replayed_spent: replayed.committed_tokens,
            replayed_fan_out_spent: replayed.fan_out_committed,
            replayed_node_spent: replayed.node_committed,
        })
    }

    /// Construct a kernel for the declared Subject kind. The legacy constructor above is
    /// explicitly whole-tree; callers carrying a pipeline definition use this entry point so
    /// an unsupported diff cannot silently execute with whole-tree semantics.
    fn for_subject(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        pipeline_version: u32,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        Kernel::new(
            cas,
            store,
            run_id,
            snapshot,
            subject,
            pipeline_version,
            authority,
        )
    }

    /// Compose execution from the exact validated pipeline definition.
    pub fn from_loaded(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
        loaded: &review_config::Loaded,
        authority: RoundAuthority,
    ) -> Result<Kernel<'a>, String> {
        let mut kernel = Kernel::for_subject(
            cas,
            store,
            run_id,
            snapshot,
            loaded.subject_kind(),
            loaded.version(),
            authority,
        )?;
        kernel.input_bindings = loaded.input_bindings();
        kernel.demand_requirements = loaded.demand_requirements().clone();
        kernel.gate_execution = loaded.gate_execution().cloned();
        kernel.integration = loaded.integration().cloned();
        kernel.reviewer_execution = loaded.reviewer_execution().clone();
        kernel.slicing = loaded
            .slicing()
            .iter()
            .map(|(node, policy)| {
                Ok((
                    node.clone(),
                    crate::scatter::StaticSlicePolicy {
                        scatter_node: policy.scatter.clone(),
                        max_paths_per_slice: policy.max_paths_per_slice,
                        max_fanout: policy.max_fanout,
                        coverage: policy.coverage,
                        all_shards_required: policy.all_shards_required,
                        closeout: policy
                            .closeout_policy()
                            .map_err(|error| error.to_string())?,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
        kernel.closeouts = loaded.closeouts().clone();
        kernel.static_node_ids = loaded.plan_order().iter().cloned().collect();
        kernel.fan_out_cap = loaded.budgets().and_then(|budgets| budgets.fan_out);
        kernel.node_attempt_caps = loaded.node_attempt_caps().clone();
        Ok(kernel)
    }

    pub fn with_checks(mut self, checks: Vec<CheckDefinition>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_check_timeout(mut self, timeout: Duration) -> Self {
        self.check_timeout = timeout;
        self
    }

    pub fn with_cache_sources(mut self, sources: BTreeMap<CacheKind, CacheSource>) -> Self {
        self.cache_sources = sources;
        self
    }

    pub fn with_cache_source_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(CacheKind) -> Result<CacheSource, CacheError> + Send + Sync + 'static,
    {
        self.cache_source_resolver = Some(Arc::new(resolver));
        self
    }

    pub fn with_container_provider(mut self, provider: ContainerProvider) -> Self {
        self.container_provider = Some(provider);
        self
    }

    pub fn with_reviewer(mut self, node_id: impl Into<String>, command: Command) -> Self {
        self.reviewers.insert(node_id.into(), Box::new(command));
        self
    }

    /// Bind a model-backed (or any other) adapter to a node, behind the same contract the
    /// `command` reviewers use.
    pub fn with_adapter(
        mut self,
        node_id: impl Into<String>,
        adapter: Box<dyn ReviewerAdapter>,
    ) -> Self {
        self.reviewers.insert(node_id.into(), adapter);
        self
    }

    /// Bind machine-local connector and credential material for one v4 brokered reviewer.
    /// Project authority names only the operation policy; these bytes remain outside snapshots,
    /// events, reviewer context, and release artifacts.
    pub fn with_broker_provider(
        mut self,
        node_id: impl Into<String>,
        provider: BrokerProvider,
    ) -> Self {
        self.broker_providers.insert(node_id.into(), provider);
        self
    }

    /// Cap the run. Reservation before every dispatch; a dispatch that cannot reserve does not
    /// happen, and the refusal names the scope that said no.
    pub fn with_budgets(mut self, attempt_cap: u64, run_cap: u64) -> Self {
        let mut ledger = BudgetLedger::default()
            .with_limit(BudgetScope::Run, Budget::of(run_cap))
            .with_committed(BudgetScope::Run, self.replayed_spent);
        let fan_out_cap = self.fan_out_cap.unwrap_or(run_cap);
        for policy in self.slicing.values() {
            let scope = BudgetScope::FanOut(policy.scatter_node.clone());
            ledger = ledger.with_limit(scope.clone(), Budget::of(fan_out_cap));
            ledger = ledger.with_committed(
                scope,
                self.replayed_fan_out_spent
                    .get(&policy.scatter_node)
                    .copied()
                    .unwrap_or(0),
            );
        }
        // A node that declared its own cap is limited at its own scope too, so a retry storm on
        // one cheap Worker cannot spend what the pipeline reserved for the deep one.
        for (node, cap) in &self.node_attempt_caps {
            ledger = ledger
                .with_limit(BudgetScope::Node(node.clone()), Budget::of(*cap))
                .with_committed(
                    BudgetScope::Node(node.clone()),
                    self.replayed_node_spent.get(node).copied().unwrap_or(0),
                );
        }
        self.budgets = Some(Budgets {
            attempt_cap,
            ledger: Mutex::new(ledger),
        });
        self
    }

    /// What one Attempt of `node_id` reserves: the node's own cap, a dynamic shard's owning
    /// Scatter cap, else the pipeline-wide attempt cap.
    fn attempt_reservation(&self, node_id: &str, budgets: &Budgets) -> u64 {
        let base = self.reviewer_binding_node(node_id);
        self.node_attempt_caps
            .get(node_id)
            .or_else(|| self.node_attempt_caps.get(&base))
            .copied()
            .unwrap_or(budgets.attempt_cap)
    }

    fn reviewer_binding_node(&self, node_id: &str) -> String {
        self.dynamic_reviewer_bases
            .lock()
            .expect("dynamic reviewer bases")
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| node_id.to_string())
    }

    /// Seed the generation-local projection with the Ledger rebuilt while its Round input was
    /// prepared. Any intervening durable suffix is folded before installation, and subsequent
    /// appends advance the watermarked cache in sequence.
    pub fn with_ledger_projection(self, mut projection: LedgerProjection) -> Result<Self, String> {
        if !projection.belongs_to(&self.run_id) {
            return Err("Ledger projection belongs to a different Campaign run".into());
        }
        {
            let store = self.store.lock().expect("event store");
            projection
                .fast_forward(*store, self.cas)
                .map_err(|error| error.to_string())?;
        }
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        Ok(self)
    }

    /// Tokens committed so far, across every attempt including fenced ones. `None` when the
    /// run is uncapped.
    pub fn spent(&self) -> Option<u64> {
        self.budgets.as_ref().map(|b| {
            b.ledger
                .lock()
                .expect("budget ledger")
                .committed(&BudgetScope::Run)
        })
    }

    /// Charge accumulated by the dynamic reviewers owned by one captured Scatter.
    pub fn fan_out_spent(&self, scatter: &str) -> Option<u64> {
        self.budgets.as_ref().map(|budgets| {
            budgets
                .ledger
                .lock()
                .expect("budget ledger")
                .committed(&BudgetScope::FanOut(scatter.to_string()))
        })
    }

    /// Every attempt this run made, quarantines included — the operator's view.
    pub fn attempts(&self) -> AttemptLedger {
        self.attempts.lock().expect("attempt ledger").clone()
    }

    /// Selected Attempt evidence for this exact Round epoch, loaded from the durable provenance
    /// artifacts referenced by `AttemptAdmitted@1`.
    pub fn selected_attempt_evidence(&self) -> Result<Vec<AttemptEvidence>, String> {
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let mut evidence = Vec::new();
        for event in events.into_iter().filter(|event| {
            event.event_type == EventType::AttemptAdmittedV1
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        }) {
            let payload: AttemptAdmittedPayloadV1 =
                serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
            if payload.selection != "selected" {
                continue;
            }
            let node = event.node_id.ok_or("selected Attempt has no node ID")?;
            let attempt_id = event
                .attempt_id
                .ok_or("selected Attempt has no Attempt ID")?;
            let provenance_id = payload
                .provenance_artifact
                .ok_or("selected Attempt has no provenance artifact")?;
            let provenance = self
                .cas
                .get_json(&provenance_id)
                .map_err(|error| error.to_string())?;
            if provenance["node"].as_str() != Some(node.as_str())
                || provenance["attempt"].as_str() != Some(attempt_id.as_str())
                || provenance["cost_tokens"].as_u64() != Some(payload.cost_tokens)
            {
                return Err("selected Attempt provenance contradicts its admission event".into());
            }
            evidence.push(AttemptEvidence {
                node,
                attempt_id,
                cost_tokens: payload.cost_tokens,
                usage: serde_json::from_value(provenance["usage"].clone())
                    .map_err(|error| error.to_string())?,
                context_manifest: serde_json::from_value(provenance["context_manifest"].clone())
                    .map_err(|error| error.to_string())?,
                raw_artifact: provenance["raw"]
                    .as_str()
                    .ok_or("selected Attempt provenance has no raw artifact")?
                    .to_string(),
                result_artifact: payload
                    .result_artifact
                    .ok_or("selected Attempt has no result artifact")?,
            });
        }
        evidence.sort_by(|left, right| {
            (&left.node, &left.attempt_id).cmp(&(&right.node, &right.attempt_id))
        });
        Ok(evidence)
    }

    /// The decision a gate node reached, if it ran.
    pub fn gate_decision(&self, node_id: &str) -> Option<GateDecision> {
        self.gates.lock().expect("gates").get(node_id).cloned()
    }

    /// The ledger as it stands, derived from the log and cached only through a run-bound
    /// projection capability.
    pub fn ledger(&self) -> Ledger {
        self.with_ledger(Ledger::clone)
    }

    fn rebuild_ledger_projection(&self) -> LedgerProjection {
        LedgerProjection::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .expect("replay")
    }

    fn with_ledger<R>(&self, inspect: impl FnOnce(&Ledger) -> R) -> R {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        if let Some(projection) = cached.as_ref() {
            return inspect(projection.ledger());
        }
        // Keep the cache lock across replay. Appends release the store lock before invalidating
        // this cache, so there is no nested inverse lock order; an invalidating append can only
        // clear the rebuilt value after it becomes visible, never race an older value back in.
        let rebuilt = self.rebuild_ledger_projection();
        let result = inspect(rebuilt.ledger());
        *cached = Some(rebuilt);
        result
    }

    fn take_ledger_projection(&self) -> LedgerProjection {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        if let Some(projection) = cached.take() {
            return projection;
        }
        // See `with_ledger`: keeping this lock closes the same stale-repopulation window.
        self.rebuild_ledger_projection()
    }

    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        self.with_ledger(|ledger| ledger.convergence(policy))
    }

    /// Append one event to the run's log. Everything the kernel decides goes through here:
    /// the log is the authority a run is rebuilt from, so a decision it never saw is a
    /// decision that, on replay, never happened.
    fn bind_authority(&self, mut event: NewEvent) -> NewEvent {
        if event.causation_id.is_none() {
            event.causation_id = Some(self.authority.round_event_id.clone());
        }
        if event.correlation_id.is_none() {
            event.correlation_id = Some(self.authority.subject_id.clone());
        }
        for artifact in self.authority.artifact_refs() {
            if !event.artifact_refs.contains(&artifact) {
                event.artifact_refs.push(artifact);
            }
        }
        event
    }

    /// Records how long one reviewer Attempt took and what its Provider reported, in the store's
    /// sidecar. A failed sidecar write must never change an Attempt's fate, so it is not an
    /// error here: the report then shows that Attempt as "not recorded".
    fn record_attempt_wall(
        &self,
        node_id: &str,
        attempt: &impl std::fmt::Display,
        started: SystemTime,
        elapsed: Duration,
        usage: Option<&review_runner::TokenUsage>,
    ) {
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let wall = review_store::AttemptWall {
            run_id: self.run_id.clone(),
            attempt_id: attempt.to_string(),
            node_id: node_id.to_string(),
            round: self.authority.round,
            epoch: self.authority.epoch,
            started_unix_ms: started.duration_since(UNIX_EPOCH).map(millis).unwrap_or(0),
            elapsed_ms: millis(elapsed),
            usage: usage.map(|usage| review_store::AttemptUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_write_tokens: usage.cache_write_tokens,
                reasoning_tokens: usage.reasoning_tokens,
                chargeable_tokens: usage.chargeable_tokens,
            }),
        };
        let _ = self
            .store
            .lock()
            .expect("event store")
            .record_attempt_wall(&wall);
    }

    fn append(&self, event: NewEvent) -> Result<(), String> {
        let event = self.bind_authority(event);
        let appended = {
            self.store
                .lock()
                .expect("event store")
                .append(&self.run_id, self.cas, event)
                .map_err(|e| e.to_string())?
        };
        self.fold_appended_into_ledger_cache(std::slice::from_ref(&appended));
        Ok(())
    }

    fn append_batch(&self, events: &[NewEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let events: Vec<NewEvent> = events
            .iter()
            .cloned()
            .map(|event| self.bind_authority(event))
            .collect();
        let appended = {
            self.store
                .lock()
                .expect("event store")
                .append_batch(&self.run_id, self.cas, &events)
                .map_err(|e| e.to_string())?
        };
        self.fold_appended_into_ledger_cache(&appended);
        Ok(())
    }

    /// M9 transitions intentionally occur after the Round has closed, so they cannot carry the
    /// Round causation installed by `append`. Keep that exception closed over the exact event
    /// vocabulary instead of exposing a general authority-bypass primitive.
    fn append_integration_transition(&self, event: NewEvent) -> Result<(), String> {
        if !matches!(
            event.event_type,
            EventType::IntegrationPreparedV1
                | EventType::IntegrationConflictV1
                | EventType::IntegrationChecksCompletedV1
        ) || event.causation_id.is_some()
        {
            return Err("invalid non-transactional Integration transition".into());
        }
        let appended = self
            .store
            .lock()
            .expect("event store")
            .append(&self.run_id, self.cas, event)
            .map_err(|error| error.to_string())?;
        self.fold_appended_into_ledger_cache(std::slice::from_ref(&appended));
        Ok(())
    }

    /// The sole M9 visibility boundary: zero or more Change Attestations immediately followed
    /// by one Integration commit. `EventStore::append_batch` supplies the SQLite transaction.
    fn commit_integration(&self, events: &[NewEvent]) -> Result<(), String> {
        let Some(last) = events.last() else {
            return Err("empty Integration commit batch".into());
        };
        if last.event_type != EventType::IntegrationCommittedV1
            || last.causation_id.is_some()
            || events[..events.len() - 1].iter().any(|event| {
                event.event_type != EventType::ChangeAttestedV1 || event.causation_id.is_some()
            })
        {
            return Err("Integration commit batch contains an unauthorized transition".into());
        }
        let appended = self
            .store
            .lock()
            .expect("event store")
            .append_batch(&self.run_id, self.cas, events)
            .map_err(|error| error.to_string())?;
        self.fold_appended_into_ledger_cache(&appended);
        Ok(())
    }

    fn fold_appended_into_ledger_cache(&self, events: &[review_core::RunEvent]) {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        let Some(projection) = cached.as_mut() else {
            return;
        };
        for event in events {
            if event.sequence < projection.event_count() {
                // A concurrent reader rebuilt through this append before we acquired the cache.
                continue;
            }
            if event.sequence > projection.event_count()
                || projection.apply_event(event, self.cas).is_err()
            {
                // Concurrent appends may reach this lock out of sequence. Dropping the cache is
                // safe; the next reader replays the exact durable log under the cache lock.
                *cached = None;
                return;
            }
        }
    }

    /// Durably record what became of every node, and the verdict derived from that. Without
    /// this the log holds the attempts but not the run's conclusion — an operator resuming
    /// from the log alone could not say what the review decided.
    pub fn publish_report(
        &self,
        report: &RunReport,
        policy: ConvergencePolicy,
    ) -> Result<RunVerdict, String> {
        let mut published = self.report_published.lock().expect("report published");
        if *published {
            return Err("this kernel generation already published its conclusion".to_string());
        }
        let prior_conclusion = {
            let store = self.store.lock().expect("event store");
            let mut conclusion = false;
            for event in store
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
            {
                match event.event_type {
                    EventType::GenerationAdvancedV1 => conclusion = false,
                    event_type if event_type.is_run_report() => {
                        conclusion = run_report_closes_round(&event)
                            .map_err(|error| error.to_string())?
                            .unwrap_or(false);
                    }
                    _ => {}
                }
            }
            conclusion
        };
        if prior_conclusion {
            return Err("this campaign generation already has a durable conclusion".to_string());
        }
        // The guaranteed flush point. `run_gather` flushes when it runs — the ordinary case,
        // and the one that keeps attempt events ahead of the findings — but a gather that was
        // suppressed (a failed reviewer upstream) or a pipeline with no gather node never
        // reaches it, and the buffered attempts, charges included, would be lost. Every run
        // ends with a report, so flushing here records the paid work no matter the graph.
        self.flush_reviewer_events()?;
        let convergence = self.convergence(policy);
        let verdict = run_verdict(report, &convergence);
        let outcomes: Vec<RunNodeReportV2> = report
            .outcomes
            .iter()
            .map(|(id, outcome)| {
                let outcome = match outcome {
                    NodeOutcome::Completed { outputs } => RunNodeOutcomeV2::Completed {
                        output_artifacts: artifact_ids(outputs),
                    },
                    NodeOutcome::Failed { error, .. } => RunNodeOutcomeV2::Failed {
                        error: error.clone(),
                    },
                    NodeOutcome::Suppressed { reason } => RunNodeOutcomeV2::Suppressed {
                        reason: run_suppression_reason(*reason),
                    },
                };
                RunNodeReportV2 {
                    node: id.clone(),
                    outcome,
                }
            })
            .collect();
        let persisted_verdict =
            persisted_verdict(&verdict, &convergence, !report.blocked_gates.is_empty())?;
        let blocked_gates = report.blocked_gates.iter().cloned().collect();
        let spent_tokens = self.spent();
        if self.gate_execution.is_some() {
            let execution_bindings: Vec<_> = self
                .execution_bindings
                .lock()
                .expect("execution bindings")
                .values()
                .cloned()
                .collect();
            let cache_snapshots: Vec<_> = self
                .cache_snapshots
                .lock()
                .expect("cache snapshots")
                .values()
                .cloned()
                .collect();
            let cache_failures: Vec<_> = self
                .cache_failures
                .lock()
                .expect("cache failures")
                .values()
                .cloned()
                .collect();
            let cache_requested = self
                .gate_execution
                .as_ref()
                .is_some_and(|binding| !binding.caches.is_empty());
            if !cache_requested {
                let payload = RunReportPayloadV4 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                };
                self.append(NewEvent::new(
                    EventType::RunReportV4,
                    serde_json::to_value(payload).map_err(|e| e.to_string())?,
                ))?;
            } else {
                let manifest_refs = cache_snapshots
                    .iter()
                    .map(|snapshot| snapshot.source_digest.clone())
                    .collect();
                let payload = RunReportPayloadV5 {
                    outcomes,
                    blocked_gates,
                    verdict: persisted_verdict,
                    spent_tokens,
                    execution_bindings,
                    cache_snapshots,
                    cache_failures,
                };
                self.append(
                    NewEvent::new(
                        EventType::RunReportV5,
                        serde_json::to_value(payload).map_err(|e| e.to_string())?,
                    )
                    .referencing(manifest_refs),
                )?;
            }
        } else {
            let payload = RunReportPayloadV3 {
                outcomes,
                blocked_gates,
                verdict: persisted_verdict,
                spent_tokens,
            };
            self.append(NewEvent::new(
                EventType::RunReportV3,
                serde_json::to_value(payload).map_err(|e| e.to_string())?,
            ))?;
        }
        *published = true;
        if verdict == RunVerdict::Pass
            && self.integration.is_some()
            && self.authority.round < self.authority.max_rounds
        {
            self.integrate_selected_proposals()?;
        }
        Ok(verdict)
    }

    /// A sandbox in the requested mode, as a copy-on-write clone of the run's single
    /// materialized template. The template is built once, under the lock, on the first call
    /// (the gate's); every later sandbox — the reviewers' — clones it instead of walking the
    /// manifest and re-reading the whole tree from the CAS.
    fn sandbox_template(&self) -> Result<Arc<review_sandbox::SandboxTemplate>, String> {
        Ok({
            let mut guard = self.template.lock().expect("template");
            match guard.as_ref() {
                Some(template) => template.clone(),
                None => {
                    let template = std::sync::Arc::new(
                        review_sandbox::SandboxTemplate::materialize(&self.snapshot, self.cas)
                            .map_err(|e| e.to_string())?,
                    );
                    *guard = Some(template.clone());
                    template
                }
            }
        })
    }

    fn sandbox(&self, mode: Mode) -> Result<Sandbox, String> {
        let template = self.sandbox_template()?;
        Sandbox::from_template(&template, mode).map_err(|e| e.to_string())
    }

    /// Emit the run's generation state — the campaign's prior findings — as the artifact a
    /// reviewer receives on its `prior_findings` input edge. In the first round there is no
    /// prior state, so an empty finding set is emitted; the edge is satisfied either way, and
    /// nothing about delivery depends on ambient kernel state.
    fn run_generation(&self, node: &Node) -> Result<ArtifactMap, String> {
        let mut outputs = ArtifactMap::new();
        for port in &node.outputs {
            let artifacts = if is_generation_prior_findings_output(port, self.pipeline_version) {
                vec![self.prior_findings.clone().ok_or(
                    "campaign execution has no exact prior Finding Set from RoundStarted@1",
                )?]
            } else if is_generation_finding_set_output(port) {
                if self.authority.prior_reduction_finding_set_id
                    == self.authority.finding_genesis_id
                {
                    Vec::new()
                } else {
                    vec![self.authority.prior_reduction_finding_set_id.clone()]
                }
            } else if is_change_set_port(port, self.pipeline_version) {
                vec![
                    self.authority
                        .change_set_id
                        .clone()
                        .ok_or("generation declares ChangeSet@1 for a whole-tree Subject")?,
                ]
            } else {
                return Err(format!(
                    "generation output `{}` has unsupported artifact type `{}`",
                    port.name, port.artifact_type
                ));
            };
            outputs.insert(port.name.clone(), artifacts);
        }
        Ok(outputs)
    }

    fn record_cache_failure(
        &self,
        node_id: &str,
        kind: CacheKind,
        reason: RunCacheFailureReasonV5,
    ) {
        let identity = (node_id.to_string(), run_cache_kind(kind));
        if self
            .cache_snapshots
            .lock()
            .expect("cache snapshots")
            .contains_key(&identity)
        {
            return;
        }
        let failure = RunCacheFailureV5 {
            node: node_id.to_string(),
            kind: run_cache_kind(kind),
            reason,
        };
        self.cache_failures
            .lock()
            .expect("cache failures")
            .entry(identity)
            .or_insert(failure);
    }

    fn record_unmaterialized_cache_failures(&self, node_id: &str, reason: RunCacheFailureReasonV5) {
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                self.record_cache_failure(node_id, kind, reason);
            }
        }
    }
}

fn validate_generation_outputs(
    authority: &RoundAuthority,
    node: &Node,
    outputs: &ArtifactMap,
    pipeline_version: u32,
) -> Result<(), String> {
    if node.kind != NodeKind::Generation {
        return Ok(());
    }
    for port in &node.outputs {
        let expected = if is_generation_prior_findings_output(port, pipeline_version) {
            vec![authority.prior_finding_set_id.clone()]
        } else if is_generation_finding_set_output(port) {
            if authority.prior_reduction_finding_set_id == authority.finding_genesis_id {
                Vec::new()
            } else {
                vec![authority.prior_reduction_finding_set_id.clone()]
            }
        } else if is_change_set_port(port, pipeline_version) {
            authority.change_set_id.iter().cloned().collect()
        } else {
            return Err(format!(
                "generation receipt port `{}` has unsupported artifact type `{}`",
                port.name, port.artifact_type
            ));
        };
        if outputs.get(&port.name) != Some(&expected) {
            return Err(format!(
                "generation receipt port `{}` contradicts Round {} authority",
                port.name, authority.round
            ));
        }
    }
    Ok(())
}

impl Dispatch for Kernel<'_> {
    fn failure_class(&self, node_id: &str) -> Option<NodeFailureClass> {
        self.failure_classes
            .lock()
            .expect("failure classes")
            .get(node_id)
            .copied()
    }

    fn record_invocation(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        let payload = NodeInvocationPayloadV1 {
            node: node.id.clone(),
            inputs: port_artifacts(&node.inputs, inputs, &self.authority.head_snapshot_id),
        };
        if let Some(recorded) = self.replayed_invocations.get(&node.id) {
            if recorded != &payload {
                return Err(format!(
                    "node `{}` no longer resolves to its durable invocation",
                    node.id
                ));
            }
        } else {
            self.append(
                NewEvent::new(
                    EventType::NodeInvocationV1,
                    serde_json::to_value(payload).map_err(|e| e.to_string())?,
                )
                .node(&node.id)
                .referencing(artifact_ids(inputs)),
            )?;
        }
        if node.kind == NodeKind::Reviewer {
            self.reviewer_input_artifacts
                .lock()
                .expect("reviewer inputs")
                .insert(node.id.clone(), artifact_ids(inputs));
        }
        Ok(())
    }

    /// The reservation and durable dispatch of a reviewer's first Attempt happen here, on the
    /// scheduler thread as the node takes its slot — never earlier, so a run cap is consumed in
    /// dispatch order and a released reservation is available to the node dispatched next.
    fn prepare_dispatch(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        if node.kind == NodeKind::Reviewer && !self.replayed_outputs.contains_key(&node.id) {
            let binding_node = self.reviewer_binding_node(&node.id);
            if !self.reviewers.contains_key(&binding_node) {
                return Err(format!("no reviewer bound to node {}", node.id));
            }
            let prior_findings = node
                .inputs
                .iter()
                .find(|port| is_reviewer_prior_set_input(port, self.pipeline_version))
                .and_then(|port| inputs.get(&port.name))
                .and_then(|artifacts| artifacts.first());
            let replayed_failures = self
                .replayed_refusal_histories
                .get(&node.id)
                .cloned()
                .unwrap_or_default();
            let prepared =
                self.prepare_reviewer_attempt(&node.id, prior_findings, &replayed_failures)?;
            self.prepared_attempts
                .lock()
                .expect("prepared attempts")
                .insert(node.id.clone(), prepared);
        }
        Ok(())
    }

    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        if let Some(receipt) = self.replayed_outputs.get(&node.id) {
            if node.kind == NodeKind::Reviewer {
                let selections = self
                    .reviewer_selections
                    .lock()
                    .expect("reviewer selections");
                let selected = selections.get(&node.id).ok_or_else(|| {
                    format!(
                        "reviewer '{}': receipt has no selected admitted attempt",
                        node.id
                    )
                })?;
                let output_artifacts: Vec<&String> = receipt
                    .payload
                    .outputs
                    .iter()
                    .flat_map(|port| &port.artifact_ids)
                    .collect();
                if receipt.attempt_id.as_deref() != Some(selected.attempt_id.as_str())
                    || output_artifacts.len() != 1
                    || output_artifacts[0] != &selected.result_artifact
                {
                    return Err(format!(
                        "reviewer '{}': receipt contradicts its selected admitted result",
                        node.id
                    ));
                }
            }
            let receipt = &receipt.payload;
            let outputs: ArtifactMap = receipt
                .outputs
                .iter()
                .map(|port| (port.port.clone(), port.artifact_ids.clone()))
                .collect();
            validate_generation_outputs(&self.authority, node, &outputs, self.pipeline_version)?;
            let expected =
                port_artifacts(&node.outputs, &outputs, &self.authority.head_snapshot_id);
            if receipt.node != node.id || receipt.outputs != expected {
                return Err(format!(
                    "node '{}': durable receipt violates its output contracts",
                    node.id
                ));
            }
            return Ok(outputs);
        }
        // Routing is on the validated kind, never the id: an id is a name someone chose, and a
        // reviewer named `gather` must still be a reviewer that runs.
        if node.kind == NodeKind::Generation {
            return self.run_generation(node);
        }
        let artifacts = match node.kind {
            NodeKind::Generation => unreachable!("generation returned above"),
            NodeKind::Gate => {
                let result = self.run_gate(&node.id);
                if result.is_err() {
                    self.record_unmaterialized_cache_failures(
                        &node.id,
                        RunCacheFailureReasonV5::GateSetupFailed,
                    );
                }
                result
            }
            NodeKind::Slicer => self.run_slicer(node),
            NodeKind::Scatter => self.run_scatter(node, inputs),
            // Gather and ledger reduce whatever artifacts their edges delivered; the port
            // labels are the reviewer's concern, not theirs.
            NodeKind::Gather => self.run_gather(node, inputs),
            NodeKind::Ledger => return self.run_ledger(node, inputs),
            NodeKind::Reviewer => self.run_reviewer(node, inputs),
        }?;
        bind_single_output(node, artifacts)
    }

    fn record_outputs(&self, node: &Node, outputs: &ArtifactMap) -> Result<(), String> {
        validate_generation_outputs(&self.authority, node, outputs, self.pipeline_version)?;
        if let Some(recorded) = self.replayed_outputs.get(&node.id) {
            let expected = NodeOutputReceiptPayloadV1 {
                node: node.id.clone(),
                outputs: port_artifacts(&node.outputs, outputs, &self.authority.head_snapshot_id),
            };
            return if recorded.payload == expected {
                self.node_outputs
                    .lock()
                    .expect("node outputs")
                    .insert(node.id.clone(), outputs.clone());
                Ok(())
            } else {
                Err(format!(
                    "node `{}` replayed outputs disagree with its durable receipt",
                    node.id
                ))
            };
        }
        let payload = NodeOutputReceiptPayloadV1 {
            node: node.id.clone(),
            outputs: port_artifacts(&node.outputs, outputs, &self.authority.head_snapshot_id),
        };
        let mut event = NewEvent::new(
            EventType::NodeOutputReceiptV1,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        )
        .node(&node.id)
        .referencing(artifact_ids(outputs));
        if node.kind == NodeKind::Reviewer {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let selected = selections.get(&node.id).ok_or_else(|| {
                format!(
                    "reviewer '{}': output has no selected admitted attempt",
                    node.id
                )
            })?;
            event = event.attempt(&selected.attempt_id);
        }
        // The scheduler publishes outputs as soon as this returns. Commit any node lifecycle
        // facts and its receipt together before that publication point. Reviewers contribute
        // attempt admission; gates contribute check results and the gate decision.
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        let mut events: Vec<NewEvent> = pending
            .iter()
            .filter(|((id, _), _)| id == &node.id)
            .map(|(_, event)| event.clone())
            .collect();
        events.push(event);
        self.append_batch(&events)?;
        pending.retain(|((id, _), _)| id != &node.id);
        self.node_outputs
            .lock()
            .expect("node outputs")
            .insert(node.id.clone(), outputs.clone());
        Ok(())
    }

    fn gate_passed(&self, node_id: &str, _outputs: &ArtifactMap) -> bool {
        self.gates
            .lock()
            .expect("gates")
            .get(node_id)
            .map(GateDecision::passed)
            .unwrap_or(false)
    }
}

impl review_config::SubjectDispatch for Kernel<'_> {
    fn subject_kind(&self) -> review_core::SubjectKind {
        self.subject
    }

    fn reviewer_credential_mode(&self, node: &str) -> Option<BrokerCredentialModeV1> {
        self.reviewers
            .get(node)
            .map(|adapter| adapter.credential_mode())
    }

    fn broker_provider_available(&self, node: &str) -> bool {
        self.broker_providers.contains_key(node)
    }
}
