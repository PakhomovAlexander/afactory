//! Canonical Review operations and durable domain facts behind the Task-hosted Review.
//! This state does not construct an Attempt ledger, reserve a budget, invoke a Worker or
//! schedule Scatter children. The Task host supplies selections.

use super::*;

pub(crate) mod integration;
mod presentation;
mod report;

fn gate_remaining(
    check_timeout: Duration,
    deadline: Option<std::time::Instant>,
) -> Result<Duration, String> {
    let remaining = deadline.map_or(check_timeout, |end| {
        check_timeout.min(end.saturating_duration_since(std::time::Instant::now()))
    });
    if remaining.is_zero() {
        Err("Review Gate exhausted its common Task Attempt deadline".into())
    } else {
        Ok(remaining)
    }
}

pub(super) struct ReviewLedgerOutputs {
    pub(super) original: ArtifactMap,
    /// The same reducer artifacts, including canonical Demands omitted by old port lists.
    pub(super) canonical: BTreeMap<String, String>,
}

pub(super) struct ReviewDomainState<'a> {
    /// One kernel generation has exactly one durable conclusion.
    report_published: Mutex<bool>,
    pub(super) cas: &'a Cas,
    pub(super) store: review_store::SharedEventStore<'a>,
    pub(super) run_id: String,
    /// The immutable subject. Every node is materialized from this, so they all inspect the
    /// same content by construction rather than by discipline.
    pub(super) snapshot: Manifest,
    pub(super) authority: RoundAuthority,
    pub(super) checks: Vec<CheckDefinition>,
    pub(super) check_timeout: Duration,
    /// Absent only for pipeline format v2. V3 resolves this exact Gate binding from captured
    /// authority before any candidate check executes.
    pub(super) gate_execution: Option<review_config::GateExecutionSpec>,
    /// Optional machine-resolved provider. The CLI normally lets the kernel probe locally;
    /// embedding callers and deterministic boundary tests may bind an already-probed provider.
    pub(super) container_provider: Option<ContainerProvider>,
    /// Machine-local sources resolved by the CLI. Host paths never enter captured pipeline
    /// authority or durable events.
    pub(super) cache_source_resolver: Option<Arc<CacheSourceResolver>>,
    pub(super) execution_bindings: Mutex<BTreeMap<String, RunExecutionBindingV4>>,
    pub(super) cache_snapshots: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheSnapshotV5>>,
    pub(super) cache_failures: Mutex<BTreeMap<(String, RunCacheKindV5), RunCacheFailureV5>>,
    /// Measured common-runtime evidence is retained with the owning Task Gate Attempt. It is
    /// separate from deterministic Review verdict artifacts and cannot affect convergence.
    pub(super) runtime_spans:
        Mutex<BTreeMap<String, Vec<review_core::task::runtime::TaskRuntimeSpanV1>>>,
    pub(super) runtime_caches:
        Mutex<BTreeMap<String, Vec<review_core::task::runtime::TaskCacheObservationV1>>>,
    pub(super) demand_requirements: BTreeMap<String, review_core::DemandRequirement>,
    pub(super) slicing: BTreeMap<String, scatter::StaticSlicePolicy>,
    pub(super) closeouts: BTreeMap<String, String>,
    pub(super) static_node_ids: BTreeSet<String>,
    /// Dynamic reviewer ID -> owning static Scatter. The binding is installed only after the
    /// complete SliceSet is durable and lets existing reviewer authority remain keyed by the
    /// captured static graph.
    pub(super) dynamic_reviewer_bases: Mutex<BTreeMap<String, String>>,
    /// Gate decisions by gate node. Keyed, so two gates in one pipeline never share a verdict.
    pub(super) gates: Mutex<BTreeMap<String, GateDecision>>,
    /// Reviewer result and gate events held until their node receipt can publish them as one
    /// batch. Each `(node, seq)` preserves emission order inside that node. Dispatch and terminal
    /// failure events are deliberately not buffered: dispatch must be durable before external
    /// execution, and a failed attempt must be durable before its retry dispatch. Their ordering
    /// across concurrently executing nodes therefore records real completion order rather than
    /// claiming whole-log determinism that the scheduler cannot provide.
    pub(super) reviewer_events: Mutex<Vec<((String, u64), NewEvent)>>,
    pub(super) reviewer_event_seq: Mutex<u64>,
    /// The snapshot materialized once, cloned per sandbox. Built lazily on the first sandbox
    /// request — the gate's — so a run that never reaches a sandbox never pays for it.
    pub(super) template: Mutex<Option<std::sync::Arc<review_sandbox::SandboxTemplate>>>,
    /// Latest projection of this generation's durable log. Every append advances its watermark,
    /// including events that leave the visible Ledger unchanged; an out-of-order concurrent
    /// observation drops the cache so the next reader rebuilds. Gather installs its live ingest.
    pub(super) ledger_cache: Mutex<Option<LedgerProjection>>,
    pub(super) reviewer_selections: Mutex<BTreeMap<String, SelectedReviewer>>,
    pub(super) reviewer_input_artifacts: Mutex<BTreeMap<String, Vec<String>>>,
    /// Validated graph provenance: downstream node -> input port -> exact upstream node, output
    /// port, and kind. The kind distinguishes reviewer selection from deterministic node output.
    pub(super) input_bindings: review_config::InputBindings,
    /// Outputs made durable in this scheduler run, keyed by their producing node. Downstream
    /// canonical reducers consult only this map plus the validated graph when binding artifacts.
    pub(super) node_outputs: Mutex<BTreeMap<String, ArtifactMap>>,
    /// Reviewer nodes with a pinned warm-layer policy. Absent nodes run cold, exactly as every
    /// pipeline written before warm layers existed.
    pub(super) warm_policies: BTreeMap<String, review_config::WarmSpec>,
    /// Reviewer nodes that carry a Build Cache, mapped to the exact Gate that captures it: the
    /// node's `gated_by`. Selection reads only that Gate's published capture, so two Gates in
    /// one pipeline can never hand a reviewer the other's build.
    pub(super) warm_build_cache_gates: BTreeMap<String, String>,
    /// Warm Sets selected and durably recorded for this Round, by node.
    pub(super) warm_sets: Mutex<BTreeMap<String, crate::warm::WarmSetRecord>>,
    /// Package P3: the machine-local root Warm Workspaces live under, resolved by the CLI to the
    /// XDG cache directory and by tests to a temporary one. `None` resolves the default at the
    /// first warm workspace. The path never enters a durable record.
    pub(super) workspace_cache_root: Option<std::path::PathBuf>,
    /// Warm Workspace templates prepared in this kernel run, by node. Each was verified to hold
    /// the Round head before it was cached here.
    pub(super) warm_workspaces: Mutex<BTreeMap<String, crate::warm::WarmWorkspace>>,
    /// Package P4: what the execution frontend hosting each node can do with the session layer,
    /// installed before the node's Warm Set is selected. An absent entry is a frontend that does
    /// not run the session protocol, and the Warm Set records `host_unsupported`.
    pub(super) session_hosts: Mutex<BTreeMap<String, crate::session::SessionCapability>>,
}

/// One Cold Closeout result folded into the Ledger beside the warm result it confirms: the
/// node both Attempts belong to, the confirmation's own result artifact and Attempt, and the
/// result value itself.
struct FoldedColdCloseout {
    /// The warm reviewer node both Attempts belong to. It is where the confirmation's exact
    /// Worker Input and its Demand requirement come from.
    node: String,
    /// The Ledger source the confirmation reduces under: the node's closeout slot, so warm and
    /// cold are two distinct stages of one reviewer rather than one source delivered twice.
    source: String,
    result_artifact_id: String,
    cold_attempt_id: String,
    result: serde_json::Value,
}

impl<'a> ReviewDomainState<'a> {
    /// Publish exact original ports and pending domain facts together, then expose them to
    /// downstream reduction. Selection was already supplied by the execution owner.
    pub(super) fn publish_outputs(
        &self,
        node: &Node,
        outputs: &ArtifactMap,
        recorded: Option<&DurableReceipt>,
    ) -> Result<(), String> {
        validate_generation_outputs(&self.authority, node, outputs)?;
        if let Some(recorded) = recorded {
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

    /// Install only graph/domain policy. Worker transport and resource ledgers belong to the
    /// caller's execution runtime; this does not replay or fence any Attempt.
    pub(super) fn configure(&mut self, loaded: &review_config::Loaded) -> Result<(), String> {
        self.input_bindings = loaded.input_bindings();
        self.demand_requirements = loaded.demand_requirements().clone();
        self.gate_execution = loaded.gate_execution().cloned();
        self.slicing = loaded
            .slicing()
            .iter()
            .map(|(node, policy)| {
                Ok((
                    node.clone(),
                    scatter::StaticSlicePolicy {
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
        self.closeouts = loaded.closeouts().clone();
        self.static_node_ids = loaded.plan_order().iter().cloned().collect();
        self.warm_policies = loaded.warm_policies().clone();
        self.warm_build_cache_gates = self
            .warm_policies
            .iter()
            .filter(|(_, policy)| !policy.build_cache.kinds().is_empty())
            .map(|(node, _)| {
                loaded
                    .planned()
                    .nodes
                    .get(node)
                    .and_then(|planned| planned.gated_by.clone())
                    .map(|gate| (node.clone(), gate))
                    .ok_or_else(|| {
                        format!(
                            "reviewer `{node}` carries a Build Cache but is not gated_by the Gate that captures it"
                        )
                    })
            })
            .collect::<Result<_, String>>()?;
        Ok(())
    }

    pub(super) fn new(
        cas: &'a Cas,
        store: review_store::SharedEventStore<'a>,
        run_id: String,
        snapshot: Manifest,
        subject: review_core::SubjectKind,
        authority: RoundAuthority,
    ) -> Result<Self, String> {
        if authority.run_id != run_id {
            return Err("Round authority belongs to a different Campaign run".into());
        }
        if snapshot.content_digest() != authority.head_content_digest {
            return Err("executed manifest does not match the Round Subject Snapshot".into());
        }
        if subject != authority.subject_kind {
            return Err("pipeline Subject kind disagrees with Round authority".into());
        }
        Ok(Self {
            report_published: Mutex::new(false),
            cas,
            store,
            run_id,
            snapshot,
            authority,
            checks: Vec::new(),
            check_timeout: Duration::from_secs(3600),
            gate_execution: None,
            container_provider: None,
            cache_source_resolver: None,
            execution_bindings: Mutex::new(BTreeMap::new()),
            cache_snapshots: Mutex::new(BTreeMap::new()),
            cache_failures: Mutex::new(BTreeMap::new()),
            runtime_spans: Mutex::new(BTreeMap::new()),
            runtime_caches: Mutex::new(BTreeMap::new()),
            demand_requirements: BTreeMap::new(),
            slicing: BTreeMap::new(),
            closeouts: BTreeMap::new(),
            static_node_ids: BTreeSet::new(),
            dynamic_reviewer_bases: Mutex::new(BTreeMap::new()),
            gates: Mutex::new(BTreeMap::new()),
            reviewer_events: Mutex::new(Vec::new()),
            reviewer_event_seq: Mutex::new(0),
            template: Mutex::new(None),
            ledger_cache: Mutex::new(None),
            reviewer_selections: Mutex::new(BTreeMap::new()),
            reviewer_input_artifacts: Mutex::new(BTreeMap::new()),
            input_bindings: BTreeMap::new(),
            node_outputs: Mutex::new(BTreeMap::new()),
            warm_policies: BTreeMap::new(),
            warm_build_cache_gates: BTreeMap::new(),
            warm_sets: Mutex::new(BTreeMap::new()),
            workspace_cache_root: None,
            warm_workspaces: Mutex::new(BTreeMap::new()),
            session_hosts: Mutex::new(BTreeMap::new()),
        })
    }
    /// Emit the run's generation state — the Campaign's exact prior `FindingSet@1`, and a diff
    /// Subject's Change Set — as the artifacts reviewers receive on their typed input edges. In
    /// the first round there is no prior reduction, so the optional Finding Set port carries
    /// nothing; nothing about delivery depends on ambient kernel state.
    pub(super) fn run_generation(&self, node: &Node) -> Result<ArtifactMap, String> {
        generation_outputs(&self.authority, node)
    }

    /// `gate_attempt` is the common Task Attempt the Gate runs under, when the Task runtime
    /// executes it; a captured Build Cache records it as producer provenance.
    pub(super) fn run_gate_controlled(
        &self,
        node_id: &str,
        deadline: Option<std::time::Instant>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        gate_attempt: Option<&str>,
    ) -> Result<Vec<String>, String> {
        crate::task::control::check(cancellation)?;

        gate_remaining(self.check_timeout, deadline)?;
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let identity = (node_id.to_string(), run_cache_kind(kind));
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .remove(&identity);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&identity);
            }
        }
        let (sandbox, container, require_unchanged) = match self.gate_execution.as_ref() {
            None => {
                // Frozen pipeline v1/v2 semantics. Those Campaigns captured no Gate Execution
                // Binding, so replay retains the old local/read-only behavior exactly.
                (self.sandbox(Mode::ReadOnly)?, None, true)
            }
            Some(binding) => {
                let mode = match binding.mode {
                    review_config::GateModeSpec::EphemeralWrite => Mode::EphemeralWrite,
                };
                let required = match binding.required_isolation {
                    review_config::IsolationSpec::None => Isolation::None,
                    review_config::IsolationSpec::Container => Isolation::Container,
                };
                let policy = Policy { require: required };
                let container = match binding.provider {
                    review_config::SandboxProviderSpec::TrustedLocal => None,
                    review_config::SandboxProviderSpec::Container => Some(
                        self.container_provider
                            .clone()
                            .unwrap_or_else(|| {
                                deadline.map_or_else(
                                    ContainerProvider::detect,
                                    ContainerProvider::detect_before,
                                )
                            })
                            .with_image(
                                binding
                                    .image
                                    .as_deref()
                                    .expect("validated container Gate image"),
                            ),
                    ),
                };
                gate_remaining(self.check_timeout, deadline)?;
                let provided = container
                    .as_ref()
                    .map_or(Isolation::None, ContainerProvider::isolation);
                let admitted = gate_provider_admitted(binding.provider, provided, required);
                // Record the resolved provider claim before materialization. A broken CAS or
                // failed clone must still leave RunReport@6 able to explain which binding was
                // selected and whether its isolation was sufficient.
                let report = RunExecutionBindingV4 {
                    node: node_id.to_string(),
                    provider: match binding.provider {
                        review_config::SandboxProviderSpec::TrustedLocal => {
                            RunExecutionProviderV4::TrustedLocal
                        }
                        review_config::SandboxProviderSpec::Container => {
                            RunExecutionProviderV4::Container
                        }
                    },
                    image: binding.image.clone(),
                    required_isolation: run_isolation(required),
                    provided_isolation: run_isolation(provided),
                    mode: RunSandboxModeV4::EphemeralWrite,
                    admitted,
                };
                self.execution_bindings
                    .lock()
                    .expect("execution bindings")
                    .insert(node_id.to_string(), report.clone());
                self.buffer_reviewer_event(
                    node_id,
                    NewEvent::new(
                        EventType::GateExecutionBoundV1,
                        serde_json::to_value(report).map_err(|error| error.to_string())?,
                    )
                    .node(node_id),
                );
                if let Some(provider) = container.as_ref() {
                    if !provider.availability().usable() {
                        return Err(format!(
                            "container provider unavailable: {}",
                            provider.availability().reason()
                        ));
                    }
                }
                let template = self.sandbox_template()?;
                let sandbox = match container.as_ref() {
                    Some(provider) => provider
                        .sandbox_from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                    None => Sandbox::from_template(&template, mode)
                        .map_err(|error| error.to_string())?,
                };
                admit(policy, &sandbox).map_err(|error| error.to_string())?;
                (sandbox, container, false)
            }
        };
        let mut cache_receipt_artifacts = Vec::new();
        let mut cache_environments = Vec::new();
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                let resolved = if let Some(resolver) = self.cache_source_resolver.as_ref() {
                    resolver(kind)
                } else {
                    Err(CacheError::new(
                        CacheErrorKind::PolicyUnavailable,
                        format!(
                            "Gate requested `{}` cache without an available machine-local mapping",
                            kind.name()
                        ),
                    ))
                };
                let source = match resolved {
                    Ok(source) if source.kind == kind => source,
                    Ok(source) => {
                        let error = CacheError::new(
                            CacheErrorKind::PolicyUnavailable,
                            format!(
                                "machine-local cache mapping for `{}` resolved as `{}`",
                                kind.name(),
                                source.kind.name()
                            ),
                        );
                        self.record_cache_failure(
                            node_id,
                            kind,
                            RunCacheFailureReasonV5::PolicyUnavailable,
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let snapshot = match materialize_cache(&source, &sandbox, self.cas) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.record_cache_failure(
                            node_id,
                            kind,
                            cache_failure_reason(error.kind()),
                        );
                        eprintln!(
                            "cache diagnostic for Gate `{node_id}`: {}",
                            error.operator_detail()
                        );
                        return Err(error.to_string());
                    }
                };
                let cache_observation_id = self
                    .cas
                    .put_json(&serde_json::json!([
                        self.run_id,
                        node_id,
                        "dependency_preparation",
                        snapshot.kind.name(),
                        snapshot.source_digest,
                        snapshot.started_unix_ms,
                        snapshot.lookup_ms,
                        snapshot.materialization_ms
                    ]))
                    .map_err(|error| error.to_string())?;
                let span_id = self
                    .cas
                    .put_json(&serde_json::json!([
                        self.run_id,
                        node_id,
                        "dependency_preparation",
                        snapshot.started_unix_ms,
                        snapshot
                            .lookup_ms
                            .saturating_add(snapshot.materialization_ms)
                    ]))
                    .map_err(|error| error.to_string())?;
                self.runtime_spans
                    .lock()
                    .expect("runtime spans")
                    .entry(node_id.to_string())
                    .or_default()
                    .push(review_core::task::runtime::TaskRuntimeSpanV1 {
                        span_id,
                        kind: review_core::task::runtime::TaskRuntimeSpanKindV1::DependencyPreparation,
                        label: snapshot.kind.name().into(),
                        started_unix_ms: snapshot.started_unix_ms,
                        elapsed_ms: snapshot.lookup_ms.saturating_add(snapshot.materialization_ms),
                    });
                self.runtime_caches
                    .lock()
                    .expect("runtime caches")
                    .entry(node_id.to_string())
                    .or_default()
                    .push(review_core::task::runtime::TaskCacheObservationV1 {
                        observation_id: cache_observation_id,
                        kind: snapshot.kind.name().into(),
                        eligible: true,
                        source_digest: snapshot.source_digest.clone(),
                        toolchain_id: None,
                        bytes_available: snapshot.bytes,
                        lookup_ms: snapshot.lookup_ms,
                        materialization_ms: snapshot.materialization_ms,
                    });
                let receipt = RunCacheSnapshotV5 {
                    node: node_id.to_string(),
                    kind: run_cache_kind(snapshot.kind),
                    source_digest: snapshot.source_digest,
                    bytes: snapshot.bytes,
                    files: snapshot.files,
                    materialization: run_cache_materialization(snapshot.materialization),
                };
                let receipt_artifact = self
                    .cas
                    .put_json(&serde_json::to_value(&receipt).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::CacheSnapshotMaterializedV1,
                        serde_json::to_value(&receipt).map_err(|error| error.to_string())?,
                    )
                    .node(node_id)
                    .referencing(vec![
                        receipt.source_digest.clone(),
                        receipt_artifact.clone(),
                    ]),
                )?;
                self.cache_snapshots
                    .lock()
                    .expect("cache snapshots")
                    .insert((node_id.to_string(), receipt.kind), receipt);
                self.cache_failures
                    .lock()
                    .expect("cache failures")
                    .remove(&(node_id.to_string(), run_cache_kind(kind)));
                cache_receipt_artifacts.push(receipt_artifact);
                cache_environments.push(kind.environment(sandbox.root()));
            }
        }
        // A trusted-local Gate that declares build caches builds into empty private
        // directories below the same reserved cache root; its checks see the pointing
        // environment. Prepared after the safe caches so the root exists exactly once.
        let build_cache_environments =
            self.prepare_gate_build_caches(node_id, &sandbox, container.is_some())?;
        let cache_root_created =
            !cache_receipt_artifacts.is_empty() || !build_cache_environments.is_empty();
        cache_environments.extend(build_cache_environments);
        // Run the checks holding no lock: each is a build or a test, and the store lock is
        // shared with every other node, so holding it across a check would stall the whole
        // pipeline for the build's duration. The lock is taken only to append each result.
        let mut runner = CheckRunner::new(self.cas, sandbox.root())
            .with_timeout(self.check_timeout)
            .with_cancellation(cancellation);
        for environment in cache_environments {
            for ((local_key, local_value), (container_key, container_value)) in
                environment.local.into_iter().zip(environment.container)
            {
                if local_key != container_key {
                    return Err("cache environment keys disagree across providers".into());
                }
                runner = runner.with_split_env(local_key, local_value, container_value);
            }
        }
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            crate::task::control::check(cancellation)?;
            runner = runner.with_timeout(gate_remaining(self.check_timeout, deadline)?);
            let mut cleanup_failure = None;
            let execution = match container.as_ref() {
                Some(provider) => runner.run_with_observed(check, |program, args, env, timeout| {
                    match provider.exec_evidenced_controlled(
                        sandbox.root(),
                        program,
                        args,
                        env,
                        timeout,
                        cancellation,
                    ) {
                        Ok(execution) => Ok((execution.output, execution.stderr_held)),
                        Err(error) => {
                            if !error.cleanup_confirmed() {
                                cleanup_failure = Some(error.to_string());
                            }
                            Err(error.to_string())
                        }
                    }
                }),
                None => runner.run_observed(check),
            };
            let result = execution.result;
            let span_id = self
                .cas
                .put_json(&serde_json::json!([
                    self.run_id,
                    node_id,
                    "check",
                    check.name,
                    execution.started_unix_ms,
                    execution.elapsed_ms
                ]))
                .map_err(|error| error.to_string())?;
            self.runtime_spans
                .lock()
                .expect("runtime spans")
                .entry(node_id.to_string())
                .or_default()
                .push(review_core::task::runtime::TaskRuntimeSpanV1 {
                    span_id,
                    kind: review_core::task::runtime::TaskRuntimeSpanKindV1::Check,
                    label: check.name.clone(),
                    started_unix_ms: execution.started_unix_ms,
                    elapsed_ms: execution.elapsed_ms,
                });
            self.buffer_reviewer_event(node_id, check_event(&result, node_id));
            results.push(result);
            if let Some(error) = cleanup_failure {
                // A container may still own the writable bind. Do not scan a concurrently
                // changing tree or delete it from under that process. Preserving this temporary
                // sandbox is the fail-closed forensic residue for an operator to recover.
                let preserved = sandbox.root().to_path_buf();
                std::mem::forget(sandbox);
                return Err(format!(
                    "container cleanup was not confirmed; Gate sandbox preserved at {}: {error}",
                    preserved.display()
                ));
            }
        }

        let decision = GateDecision::evaluate(&results);
        if decision.passed() {
            // Only a passing Gate dispatches reviewers, so only a passing Gate's build output
            // is worth carrying. The capture records its own refusals; it never changes the
            // verdict.
            self.capture_gate_build_caches(node_id, gate_attempt, &sandbox)?;
        }
        if cache_root_created {
            remove_materialized_caches(&sandbox)?;
        }
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if require_unchanged && !sealed.unchanged() {
            // Frozen v1/v2 behavior: those Gates promised read-only execution. V3 deliberately
            // permits writes in this one disposable clone; reviewer clones still start from the
            // pristine template, so Gate mutations cannot become Subject content.
            let paths = sealed.mutations.paths();
            return Err(format!(
                "gate mutated its read-only sandbox: {} paths, e.g. {:?}",
                paths.len(),
                &paths[..paths.len().min(20)]
            ));
        }
        let artifact = self
            .cas
            .put_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut event_artifacts = vec![artifact.clone()];
        event_artifacts.extend(cache_receipt_artifacts);
        if !require_unchanged {
            // V3 Gates may write only inside their disposable clone. Preserve what they wrote
            // before that clone disappears: the complete mutation set lives once in the CAS,
            // while the Gate event references the same bounded summary shape as Worker
            // provenance. The decision artifact remains the Gate's sole graph output.
            let mutations_artifact = self
                .cas
                .put_json(&serde_json::json!({
                    "added": sealed.mutations.added,
                    "modified": sealed.mutations.modified,
                    "deleted": sealed.mutations.deleted,
                }))
                .map_err(|error| error.to_string())?;
            let mutation_summary = mutation_summary(&sealed.mutations, &mutations_artifact);
            let mutation_summary_artifact = self
                .cas
                .put_json(&mutation_summary)
                .map_err(|error| error.to_string())?;
            event_artifacts.push(mutation_summary_artifact);
        }
        self.buffer_reviewer_event(
            node_id,
            NewEvent::new(
                EventType::GateDecisionV1,
                serde_json::to_value(&decision).map_err(|e| e.to_string())?,
            )
            .node(node_id)
            .referencing(event_artifacts),
        );
        self.gates
            .lock()
            .expect("gates")
            .insert(node_id.to_string(), decision);
        Ok(vec![artifact])
    }

    pub(super) fn run_slicer(&self, node: &Node) -> Result<Vec<String>, String> {
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

    /// A real gather: one artifact holding exactly the report artifacts the edges delivered.
    /// A reviewer whose result port feeds no edge is absent here, and therefore absent from
    /// everything downstream — the plan is the data flow, not a suggestion about it.
    ///
    /// This is also the run's canonical barrier: every reviewer has finished, so the buffered
    /// reviewer events are flushed here in node order, giving the log a shape that is a
    /// function of the pipeline rather than of thread timing.
    pub(super) fn run_gather(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        self.flush_reviewer_events()?;
        let sources = self
            .input_bindings
            .get(&node.id)
            .ok_or_else(|| format!("canonical gather `{}` has no pinned input graph", node.id))?;
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
                    let Some(index) = remaining.iter().position(|delivered| delivered == artifact)
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
        Ok(vec![artifact])
    }

    /// One Cold Closeout result, ready to fold beside the warm result it confirms.
    ///
    /// The Cold Closeout results this Round recorded for the warm results `results` already
    /// holds: for each delivered reviewer result, the cold confirmation of that exact artifact,
    /// attributed to the same node. A closeout that failed contributes nothing to reduce; its
    /// record still says the Round has no cold confirmation.
    fn cold_closeout_results(
        &self,
        results: &[(String, String, LegacyStageOutput)],
    ) -> Result<Vec<FoldedColdCloseout>, String> {
        let delivered: BTreeSet<(&str, &str)> = results
            .iter()
            .map(|(node, id, _)| (node.as_str(), id.as_str()))
            .collect();
        if delivered.is_empty() {
            return Ok(Vec::new());
        }
        let events = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?;
        let mut folded = Vec::new();
        for event in events.iter().filter(|event| {
            event.event_type == EventType::ColdCloseoutDispatchedV1
                && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
        }) {
            let payload: review_core::ColdCloseoutDispatchedPayloadV1 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            payload.validate()?;
            if !delivered.contains(&(
                payload.node.as_str(),
                payload.warm_result_artifact_id.as_str(),
            )) {
                continue;
            }
            let Some(result_id) = payload.cold_result_artifact_id else {
                // A required confirmation that produced no admissible result leaves the Round
                // without the cold opinion its policy compiled. The Ledger refuses to reduce
                // rather than let the warm result close the Round alone.
                return Err(format!(
                    "node `{}` owes a cold confirmation that produced no admissible result: {}",
                    payload.node,
                    payload
                        .failed
                        .as_deref()
                        .unwrap_or("the Cold Closeout Attempt recorded no reason")
                ));
            };
            let result = self
                .cas
                .get_json(&result_id)
                .map_err(|error| error.to_string())?;
            folded.push(FoldedColdCloseout {
                source: crate::closeout::closeout_slot(&payload.node),
                node: payload.node,
                result_artifact_id: result_id,
                cold_attempt_id: payload.cold_attempt_id,
                result,
            });
        }
        Ok(folded)
    }

    pub(super) fn reduce_ledger(
        &self,
        node: &Node,
        inputs: &ArtifactMap,
    ) -> Result<ReviewLedgerOutputs, String> {
        // The ledger reduces what its edges delivered — never a global map of whatever happened
        // to run. Each input is one reviewer's result, or a gather manifest of result ids.
        let mut results: Vec<(String, String, LegacyStageOutput)> = Vec::new();
        let mut dynamic_sets: Vec<(String, String, SliceSetV1, ShardSetV1)> = Vec::new();
        let mut direct_sources_used = BTreeSet::new();
        let mut load = |node: &str, id: &str, value: serde_json::Value| -> Result<(), String> {
            let output =
                reviewer_stage_output(value).map_err(|error| format!("artifact {id}: {error}"))?;
            results.push((node.to_string(), id.to_string(), output));
            Ok(())
        };
        for (input_port, artifacts) in inputs {
            for input in artifacts {
                let value = self.cas.get_json(input).map_err(|e| e.to_string())?;
                if value.get("type").and_then(serde_json::Value::as_str)
                    == Some(review_core::contract::SHARD_SET_V1)
                {
                    let envelope: review_core::ArtifactEnvelope = serde_json::from_value(value)
                        .map_err(|error| format!("ShardSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&envelope)?;
                    if envelope.subject_snapshot_id.as_deref()
                        != Some(self.authority.head_snapshot_id.as_str())
                    {
                        return Err(
                            "Ledger received a ShardSet outside current Subject authority".into(),
                        );
                    }
                    let scatter_node = match &envelope.producer {
                        Producer::KernelOperation {
                            node_id: Some(node),
                            ..
                        } => node.clone(),
                        _ => return Err("ShardSet@1 was not produced by its static Scatter".into()),
                    };
                    let shard_set: ShardSetV1 = serde_json::from_value(envelope.payload)
                        .map_err(|error| format!("ShardSet@1 payload is malformed: {error}"))?;
                    let slice_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(&shard_set.slice_set_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| format!("SliceSet@1 envelope is malformed: {error}"))?;
                    review_store::validate_envelope(&slice_envelope)?;
                    if slice_envelope.artifact_type != review_core::contract::SLICE_SET_V1 {
                        return Err("ShardSet@1 references a non-SliceSet artifact".into());
                    }
                    let slice_set: SliceSetV1 = serde_json::from_value(slice_envelope.payload)
                        .map_err(|error| format!("SliceSet@1 payload is malformed: {error}"))?;
                    shard_set.validate_against(&slice_set)?;
                    if slice_set.all_shards_required && !shard_set.complete() {
                        let failures = shard_set
                            .shards
                            .iter()
                            .filter_map(|shard| match &shard.outcome {
                                ShardOutcomeV1::Failed { reason }
                                | ShardOutcomeV1::Missing { reason } => {
                                    Some(format!("{}: {reason}", shard.runtime_node_id))
                                }
                                ShardOutcomeV1::Completed { .. } => None,
                            })
                            .collect::<Vec<_>>()
                            .join("; ");
                        return Err(format!(
                            "Scatter `{scatter_node}` has a failed or missing required shard: {failures}"
                        ));
                    }
                    for shard in &shard_set.shards {
                        if let ShardOutcomeV1::Completed {
                            result_artifact_ids,
                        } = &shard.outcome
                        {
                            for result_id in result_artifact_ids {
                                let result = self
                                    .cas
                                    .get_json(result_id)
                                    .map_err(|error| error.to_string())?;
                                load(&shard.runtime_node_id, result_id, result)?;
                            }
                        }
                    }
                    dynamic_sets.push((scatter_node, input.clone(), slice_set, shard_set));
                    continue;
                }
                if value.get("verdict").is_some() && value.get("reports").is_some() {
                    let upstream = self
                        .input_bindings
                        .get(&node.id)
                        .and_then(|ports| ports.get(input_port))
                        .ok_or_else(|| {
                            format!(
                                "canonical ledger `{}.{input_port}` has no pinned input graph",
                                node.id
                            )
                        })?;
                    let selections = self
                        .reviewer_selections
                        .lock()
                        .expect("reviewer selections");
                    let selected = upstream.iter().find(|(source, _, kind)| {
                        *kind == NodeKind::Reviewer
                            && !direct_sources_used.contains(source)
                            && selections
                                .get(source)
                                .is_some_and(|selection| selection.result_artifact == *input)
                    });
                    let source = selected.map(|(source, _, _)| source.clone()).ok_or_else(|| {
                        format!(
                            "canonical ledger `{}.{input_port}` cannot bind delivered result {input} to its pinned upstream reviewers",
                            node.id
                        )
                    })?;
                    direct_sources_used.insert(source.clone());
                    load(&source, input, value)?;
                    continue;
                }
                match value {
                    serde_json::Value::Object(manifest) => {
                        for (node, ids) in manifest {
                            let ids = ids.as_array().ok_or_else(|| {
                                format!("gather manifest {input} has a non-array port")
                            })?;
                            for id in ids {
                                let id = id.as_str().ok_or_else(|| {
                                    format!("gather manifest {input} holds a non-id")
                                })?;
                                let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
                                load(&node, id, value)?;
                            }
                        }
                    }
                    _ => {
                        return Err(format!(
                            "artifact {input} is neither a supported ReviewerResult nor a gather manifest"
                        ));
                    }
                }
            }
        }
        // Label each delivered copy of a selected result with the node whose Attempt selected
        // it. The selection lock is released before the Attempt bindings below take it again.
        {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let mut result_indices: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, (_, result_id, _)) in results.iter().enumerate() {
                result_indices
                    .entry(result_id.clone())
                    .or_default()
                    .push(index);
            }
            for (result_id, indices) in result_indices {
                let mut assigned = BTreeSet::new();
                let mut unmatched = Vec::new();
                for index in indices {
                    let node = &results[index].0;
                    if selections
                        .get(node)
                        .is_some_and(|selection| selection.result_artifact == result_id)
                    {
                        if !assigned.insert(node.clone()) {
                            return Err(format!(
                                "selected reviewer result for `{node}` was delivered more than once"
                            ));
                        }
                    } else {
                        unmatched.push(index);
                    }
                }
                let mut selected: Vec<_> = selections
                    .iter()
                    .filter(|(node, selection)| {
                        selection.result_artifact == result_id && !assigned.contains(*node)
                    })
                    .map(|(node, _)| node.clone())
                    .collect();
                if selected.len() < unmatched.len() {
                    return Err(format!(
                        "selected reviewer result {result_id} has {} unlabelled delivered copies and {} unused matching Attempts",
                        unmatched.len(),
                        selected.len(),
                    ));
                }
                selected.sort();
                unmatched.sort_by(|left, right| {
                    (&results[*left].0, *left).cmp(&(&results[*right].0, *right))
                });
                for (index, node) in unmatched.into_iter().zip(selected) {
                    results[index].0 = node;
                }
            }
        }
        // Package P4: a Cold Closeout is a second Attempt of the same node inside this Round,
        // and its result folds beside the exact warm result an edge delivered. The closure is
        // anchored to that delivered artifact, so the Ledger still reduces what its edges
        // delivered rather than a global scan of whatever happened to run. It is folded after
        // the selection binding above, which pairs delivered artifacts with selected Attempts:
        // a confirmation is not the node's selected result and never competes for that slot.
        // Keyed by the closeout's own Ledger source, never by its result digest: a
        // confirmation that answers exactly as the warm Attempt did is the same content and
        // would otherwise be mistaken for it.
        let mut closeout_attempts: BTreeMap<String, (String, String)> = BTreeMap::new();
        for closeout in self.cold_closeout_results(&results)? {
            let output = reviewer_stage_output(closeout.result)
                .map_err(|error| format!("artifact {}: {error}", closeout.result_artifact_id))?;
            closeout_attempts.insert(
                closeout.source.clone(),
                (closeout.cold_attempt_id, closeout.node),
            );
            results.push((closeout.source, closeout.result_artifact_id, output));
        }
        // Canonical gather order: reviewer node id — not completion order, input-port label, or
        // artifact digest order.
        results.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

        let attempt_bindings = {
            let selections = self
                .reviewer_selections
                .lock()
                .expect("reviewer selections");
            let reviewer_inputs = self
                .reviewer_input_artifacts
                .lock()
                .expect("reviewer inputs");
            results
                .iter()
                .map(|(node, result_id, _)| {
                    // A Cold Closeout result belongs to its own Attempt under its own
                    // slot, run on the warm node's exact invocation inputs. It is not the
                    // node's selection, so it is bound from its own durable record instead.
                    if let Some((attempt_id, warm_node)) = closeout_attempts.get(node) {
                        return Ok((
                            attempt_id.clone(),
                            reviewer_inputs
                                .get(node)
                                .or_else(|| reviewer_inputs.get(warm_node))
                                .cloned()
                                .ok_or_else(|| {
                                    format!(
                                        "Cold Closeout of `{warm_node}` has no exact invocation inputs"
                                    )
                                })?,
                        ));
                    }
                    let selection = selections.get(node).ok_or_else(|| {
                        format!("selected reviewer result for `{node}` has no Attempt")
                    })?;
                    if &selection.result_artifact != result_id {
                        return Err(format!(
                            "selected reviewer result for `{node}` disagrees with its Attempt"
                        ));
                    }
                    Ok((
                        selection.attempt_id.clone(),
                        reviewer_inputs.get(node).cloned().ok_or_else(|| {
                            format!("selected reviewer `{node}` has no exact invocation inputs")
                        })?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?
        };

        let projection = self.take_ledger_projection();
        let (
            round,
            finding_entries,
            grouping_relation_ids,
            grouping_input_artifact_ids,
            resolution_ids,
            resolution_input_artifact_ids,
            demand_entries,
            demand_artifact_ids,
            demand_input_artifact_ids,
            reduction,
            mut projection,
        ) = {
            let mut store = self.store.lock().expect("event store");
            let mut ingest =
                Ingest::from_projection(*store, self.cas, self.run_id.clone(), projection)
                    .map_err(|e| e.to_string())?
                    .under_round(&self.authority.round_event_id);
            let reduction_round =
                canonical_reduction_round(ingest.ledger().round, self.authority.round)?;
            let stages: Vec<_> = results
                .iter()
                .zip(&attempt_bindings)
                .map(
                    |((node, result_id, stage), (attempt_id, input_artifacts))| {
                        // A confirmation answers for the reviewer it confirms, so its
                        // Demand requirement is that reviewer's, not its slot's.
                        let subject = closeout_attempts
                            .get(node)
                            .map_or(node.as_str(), |(_, warm)| warm.as_str());
                        let binding_node = self.reviewer_binding_node(subject);
                        review_store::CanonicalStage {
                            source: node,
                            demand_requirement: self
                                .demand_requirements
                                .get(&binding_node)
                                .copied()
                                .unwrap_or(review_core::DemandRequirement::Required),
                            stage,
                            attempt_id,
                            result_artifact_id: result_id,
                            input_artifacts,
                            subject_snapshot_id: &self.authority.head_snapshot_id,
                            subject_id: &self.authority.subject_id,
                        }
                    },
                )
                .collect();
            let reduction = ingest
                .add_canonical_stage_outputs(&stages)
                .map_err(|error| error.to_string())?;
            (
                reduction_round,
                finding_set_entries(ingest.ledger()),
                ingest.ledger().grouping_relation_ids(),
                ingest.ledger().grouping_input_artifact_ids(),
                ingest.ledger().resolution_artifact_ids(),
                ingest.ledger().resolution_input_artifact_ids(),
                ingest.ledger().demand_views(),
                ingest.ledger().demand_reduction_artifact_ids(),
                ingest.ledger().demand_reduction_input_ids(),
                reduction,
                ingest.into_projection(),
            )
        };
        let proposal_ids = self.finalize_proposals(&reduction)?;
        projection
            .fast_forward(*self.store.lock().expect("event store"), self.cas)
            .map_err(|error| error.to_string())?;
        let semantic_reduction_outputs = (
            reduction.selected_report_ids.clone(),
            reduction.relation_ids.clone(),
            reduction.selected_demand_artifact_ids.clone(),
        );
        let semantic_grouping_ids = grouping_relation_ids.clone();
        let semantic_resolution_ids = resolution_ids.clone();
        let semantic_demand_lifecycle_ids = demand_artifact_ids.clone();
        *self.ledger_cache.lock().expect("ledger cache") = Some(projection);
        let reducer_version = review_core::FINDING_REDUCER_VERSION_V2;
        let payload = review_core::FindingSetV1 {
            subject_id: self.authority.subject_id.clone(),
            round,
            prior_finding_set_id: self.authority.prior_reduction_finding_set_id.clone(),
            reducer_version: reducer_version.to_string(),
            identity_policy: self.authority.finding_identity_policy.clone(),
            selected_report_ids: reduction.selected_report_ids,
            relation_ids: reduction
                .relation_ids
                .into_iter()
                .chain(grouping_relation_ids)
                .collect(),
            resolution_ids,
            findings: finding_entries,
        };
        payload.validate()?;
        let mut reduction_inputs = vec![self.authority.prior_reduction_finding_set_id.clone()];
        reduction_inputs.extend(reduction.input_artifact_ids);
        reduction_inputs.extend(grouping_input_artifact_ids);
        reduction_inputs.extend(resolution_input_artifact_ids);
        let operation_digest = review_store::content_id(&serde_json::json!({
            "reducer_version": reducer_version,
            "identity_policy": self.authority.finding_identity_policy,
            "inputs": reduction_inputs,
        }))
        .map_err(|error| error.to_string())?;
        let (findings_artifact, _) = self
            .cas
            .put_artifact(
                review_core::contract::FINDING_SET_V1,
                review_core::Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some("ledger".into()),
                    operation_id: format!(
                        "{}:{}:{}",
                        reducer_version, self.authority.finding_identity_policy, operation_digest
                    ),
                },
                reduction_inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(payload).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;

        let mut canonical_outputs =
            BTreeMap::from([("finding_set".to_string(), findings_artifact.clone())]);
        let finding_port = node
            .outputs
            .iter()
            .find(|port| is_generation_finding_set_output(port))
            .or_else(|| (node.outputs.len() == 1).then(|| &node.outputs[0]))
            .ok_or_else(|| "ledger node has no Finding Set output".to_string())?;
        let mut outputs = ArtifactMap::from([(finding_port.name.clone(), vec![findings_artifact])]);

        if round == 1 && self.authority.prior_demand_set_id != self.authority.demand_genesis_id {
            return Err("Round 1 Demand Set does not descend from Campaign genesis".into());
        }
        let (selected_demand_artifact_ids, satisfaction_artifact_ids, waiver_artifact_ids) =
            demand_artifact_ids;
        let payload = review_core::DemandSetV1 {
            subject_id: self.authority.subject_id.clone(),
            round,
            prior_demand_set_id: self.authority.prior_demand_set_id.clone(),
            reducer_version: review_core::DEMAND_REDUCER_VERSION.into(),
            selected_demand_artifact_ids,
            satisfaction_artifact_ids,
            waiver_artifact_ids,
            demands: demand_entries,
        };
        payload.validate()?;
        let mut reduction_inputs = vec![self.authority.prior_demand_set_id.clone()];
        reduction_inputs.extend(demand_input_artifact_ids);
        let mut unique = BTreeSet::new();
        reduction_inputs.retain(|id| unique.insert(id.clone()));
        let operation_digest = review_store::content_id(&serde_json::json!({
            "reducer_version": review_core::DEMAND_REDUCER_VERSION,
            "inputs": reduction_inputs,
        }))
        .map_err(|error| error.to_string())?;
        let (record_id, _) = self
            .cas
            .put_artifact(
                review_core::contract::DEMAND_SET_V1,
                review_core::Producer::KernelOperation {
                    run_id: self.run_id.clone(),
                    node_id: Some("ledger".into()),
                    operation_id: format!(
                        "{}:{}",
                        review_core::DEMAND_REDUCER_VERSION,
                        operation_digest
                    ),
                },
                reduction_inputs,
                Some(self.authority.head_snapshot_id.clone()),
                serde_json::to_value(payload).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        canonical_outputs.insert("demand_set".into(), record_id.clone());
        if let Some(port) = node.outputs.iter().find(|port| is_demand_set_port(port)) {
            outputs.insert(port.name.clone(), vec![record_id]);
        }
        if !dynamic_sets.is_empty() {
            let delivered_results = results
                .iter()
                .map(|(source, artifact, _)| (source.clone(), artifact.clone()))
                .collect::<BTreeSet<_>>();
            let round_events = self
                .store
                .lock()
                .expect("event store")
                .replay(&self.run_id)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter(|event| {
                    event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
                })
                .collect::<Vec<_>>();
            for (scatter_node, shard_record, slice_set, shard_set) in dynamic_sets {
                let mut selected = Vec::new();
                for shard in &shard_set.shards {
                    if let ShardOutcomeV1::Completed {
                        result_artifact_ids,
                    } = &shard.outcome
                    {
                        for artifact in result_artifact_ids {
                            if !delivered_results
                                .contains(&(shard.runtime_node_id.clone(), artifact.clone()))
                            {
                                return Err(format!(
                                    "dynamic result `{artifact}` from `{}` did not reach Ledger",
                                    shard.runtime_node_id
                                ));
                            }
                            selected.push(artifact.clone());
                        }
                    }
                }
                let selected_outputs = BTreeMap::from([("reviewer_results".into(), selected)]);
                let mut sinks = selected_outputs
                    .values()
                    .flatten()
                    .map(|artifact| (artifact.clone(), "ledger:reviewer-result".into()))
                    .collect::<BTreeMap<String, String>>();
                let closeout_result_id = match &slice_set.closeout {
                    review_core::CloseoutPolicyV1::Required => {
                        let closeout = self.closeouts.get(&scatter_node).ok_or_else(|| {
                            format!("Scatter `{scatter_node}` has no captured closeout reviewer")
                        })?;
                        let ids = results
                            .iter()
                            .filter(|(source, _, _)| source == closeout)
                            .map(|(_, artifact, _)| artifact.clone())
                            .collect::<Vec<_>>();
                        let [artifact] = ids.as_slice() else {
                            return Err(format!(
                                "closeout reviewer `{closeout}` did not deliver exactly one result"
                            ));
                        };
                        sinks.insert(artifact.clone(), "ledger:whole-subject-closeout".into());
                        Some(artifact.clone())
                    }
                    review_core::CloseoutPolicyV1::Waived { policy_id, .. } => {
                        if !self.authority.policy_ids.contains(policy_id) {
                            return Err(format!(
                                "closeout waiver `{policy_id}` is absent from Authority Snapshot"
                            ));
                        }
                        None
                    }
                };
                let mut closure = scatter::prove_semantic_closure(
                    &slice_set,
                    &shard_set,
                    &selected_outputs,
                    &sinks,
                    closeout_result_id.clone(),
                )?;
                let mut semantic_sinks = closure
                    .dispositions
                    .iter()
                    .map(|disposition| (disposition.artifact_id.clone(), disposition.sink.clone()))
                    .collect::<BTreeMap<_, _>>();
                if let Some(closeout) = closeout_result_id {
                    semantic_sinks.insert(closeout, "ledger:whole-subject-closeout".into());
                }
                let (reports, relations, demands) = &semantic_reduction_outputs;
                for artifact in reports {
                    semantic_sinks.insert(artifact.clone(), "ledger:finding-set".into());
                }
                for artifact in relations {
                    semantic_sinks.insert(artifact.clone(), "ledger:relation".into());
                }
                for artifact in demands {
                    semantic_sinks.insert(artifact.clone(), "ledger:demand-set".into());
                }
                for artifact in &semantic_grouping_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:grouping".into());
                }
                for artifact in &semantic_resolution_ids {
                    semantic_sinks.insert(artifact.clone(), "ledger:resolution".into());
                }
                for artifact in semantic_demand_lifecycle_ids
                    .0
                    .iter()
                    .chain(&semantic_demand_lifecycle_ids.1)
                    .chain(&semantic_demand_lifecycle_ids.2)
                {
                    semantic_sinks.insert(artifact.clone(), "ledger:demand-lifecycle".into());
                }
                for proposal_id in &proposal_ids {
                    semantic_sinks.insert(proposal_id.clone(), "proposal-store".into());
                    let proposal_envelope: review_core::ArtifactEnvelope = serde_json::from_value(
                        self.cas
                            .get_json(proposal_id)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                    let proposal: review_core::PatchProposal =
                        serde_json::from_value(proposal_envelope.payload)
                            .map_err(|error| error.to_string())?;
                    for evidence in proposal.evidence_ids {
                        semantic_sinks.insert(evidence, "proposal-store:evidence".into());
                    }
                }
                for event in &round_events {
                    let sink = match event.event_type {
                        EventType::CheckCompletedV1 => Some("event-log:check"),
                        EventType::GateDecisionV1 => Some("event-log:policy"),
                        EventType::EvidenceAddedV1 => Some("ledger:evidence"),
                        _ => None,
                    };
                    if let Some(sink) = sink {
                        let artifact = self
                            .cas
                            .put_json(&event.payload)
                            .map_err(|error| error.to_string())?;
                        semantic_sinks.insert(artifact, sink.into());
                    }
                }
                closure.required_artifact_ids = semantic_sinks.keys().cloned().collect();
                closure.dispositions = semantic_sinks
                    .into_iter()
                    .map(|(artifact_id, sink)| review_core::SemanticDispositionV1 {
                        artifact_id,
                        sink,
                    })
                    .collect();
                closure.validate()?;
                let mut closure_inputs = vec![shard_record.clone(), shard_set.slice_set_id.clone()];
                closure_inputs.extend(closure.required_artifact_ids.iter().cloned());
                closure_inputs.sort();
                closure_inputs.dedup();
                for artifact in &closure_inputs {
                    self.cas.verify(artifact).map_err(|error| {
                        format!("semantic closure input `{artifact}` is not durable: {error}")
                    })?;
                }
                let operation_id = review_store::content_id(
                    &serde_json::to_value(&closure).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
                let (record_id, _) = self
                    .cas
                    .put_artifact(
                        review_core::contract::SEMANTIC_CLOSURE_V1,
                        Producer::KernelOperation {
                            run_id: self.run_id.clone(),
                            node_id: Some(node.id.clone()),
                            operation_id,
                        },
                        closure_inputs,
                        Some(self.authority.head_snapshot_id.clone()),
                        serde_json::to_value(closure).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                self.append(
                    NewEvent::new(
                        EventType::SemanticClosureCheckedV1,
                        serde_json::to_value(RecordedSetPayloadV1 {
                            artifact_id: record_id.clone(),
                            record_id: record_id.clone(),
                        })
                        .map_err(|error| error.to_string())?,
                    )
                    .node(&node.id)
                    .referencing(vec![record_id]),
                )?;
            }
        }
        Ok(ReviewLedgerOutputs {
            original: outputs,
            canonical: canonical_outputs,
        })
    }

    pub(super) fn finalize_proposals(
        &self,
        reduction: &review_store::CanonicalReduction,
    ) -> Result<Vec<String>, String> {
        let existing = self
            .store
            .lock()
            .expect("event store")
            .replay(&self.run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| {
                event.event_type == EventType::ProposalAcceptedV1
                    && event.causation_id.as_deref() == Some(self.authority.round_event_id.as_str())
            })
            .map(|event| {
                let payload: ProposalAcceptedPayloadV1 =
                    serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
                Ok((payload.candidate_artifact_id.clone(), payload))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let selections = self
            .reviewer_selections
            .lock()
            .expect("reviewer selections")
            .clone();
        let mut proposal_ids = Vec::new();
        let mut events = Vec::new();
        for (node, selection) in selections {
            let Some(candidate_artifact) = selection.proposal_candidate else {
                continue;
            };
            if let Some(accepted) = existing.get(&candidate_artifact) {
                self.cas
                    .verify(&accepted.proposal_artifact_id)
                    .map_err(|error| error.to_string())?;
                proposal_ids.push(accepted.proposal_id.clone());
                continue;
            }
            let candidate: ProposalCandidateV1 = serde_json::from_value(
                self.cas
                    .get_json(&candidate_artifact)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            candidate.validate().map_err(str::to_string)?;
            if candidate.result_artifact_id != selection.result_artifact
                || candidate.base_snapshot_id != self.authority.head_snapshot_id
            {
                return Err(format!(
                    "prepared Proposal for `{node}` contradicts selected Round authority"
                ));
            }
            let source_reports = reduction
                .report_ids_by_source
                .get(&node)
                .ok_or_else(|| format!("prepared Proposal source `{node}` reached no Reports"))?;
            let mut finding_refs = candidate
                .finding_ids
                .iter()
                .map(|id| review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Finding,
                    id: id.clone(),
                })
                .collect::<Vec<_>>();
            for index in &candidate.report_indexes {
                let report_id = source_reports
                    .get(*index as usize)
                    .ok_or_else(|| {
                        format!("prepared Proposal for `{node}` names absent Report {index}")
                    })?
                    .clone();
                finding_refs.push(review_core::ClaimRef {
                    kind: review_core::ClaimRefKind::Report,
                    id: report_id,
                });
            }
            let proposal = review_core::PatchProposal {
                base_snapshot_id: candidate.base_snapshot_id.clone(),
                patch_artifact_id: candidate.patch_artifact_id.clone(),
                finding_refs,
                evidence_ids: candidate.evidence_ids.clone(),
                paths: candidate.paths.clone(),
                description: candidate.description.clone(),
                auto_apply_nominated: candidate.auto_apply_nominated,
            };
            proposal.check_shape().map_err(str::to_string)?;
            let mut inputs = vec![
                candidate_artifact.clone(),
                candidate.result_artifact_id.clone(),
                candidate.patch_artifact_id.clone(),
                candidate.derived_manifest_artifact_id.clone(),
            ];
            inputs.extend(candidate.evidence_ids.iter().cloned());
            inputs.sort();
            inputs.dedup();
            let (proposal_artifact_id, envelope) = self
                .cas
                .put_artifact(
                    review_core::contract::PATCH_PROPOSAL_V1,
                    Producer::Attempt {
                        run_id: self.run_id.clone(),
                        node_id: node.clone(),
                        attempt_id: selection.attempt_id.clone(),
                    },
                    inputs,
                    Some(candidate.base_snapshot_id.clone()),
                    serde_json::to_value(proposal).map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?;
            let payload = ProposalAcceptedPayloadV1 {
                proposal_id: envelope.artifact_id.clone(),
                proposal_artifact_id: proposal_artifact_id.clone(),
                candidate_artifact_id: candidate_artifact.clone(),
            };
            proposal_ids.push(envelope.artifact_id);
            events.push(
                NewEvent::new(
                    EventType::ProposalAcceptedV1,
                    serde_json::to_value(payload).map_err(|error| error.to_string())?,
                )
                .node(node)
                .attempt(selection.attempt_id)
                .referencing(vec![proposal_artifact_id, candidate_artifact]),
            );
        }
        self.append_batch(&events)?;
        proposal_ids.sort();
        proposal_ids.dedup();
        Ok(proposal_ids)
    }

    /// A sandbox in the requested mode, as a copy-on-write clone of the run's single
    /// materialized template. The template is built once, under the lock, on the first call
    /// (the gate's); every later sandbox — the reviewers' — clones it instead of walking the
    /// manifest and re-reading the whole tree from the CAS.
    pub(super) fn sandbox_template(&self) -> Result<Arc<review_sandbox::SandboxTemplate>, String> {
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

    pub(super) fn sandbox(&self, mode: Mode) -> Result<Sandbox, String> {
        let template = self.sandbox_template()?;
        Sandbox::from_template(&template, mode).map_err(|e| e.to_string())
    }

    /// A fresh sandbox for one node's Attempt: a clone of the node's Warm Workspace template
    /// when its policy keeps one, and of the run's temporary template otherwise. Either way
    /// every Attempt gets its own copy-on-write clone and nothing it writes reaches a sibling,
    /// the template or the source.
    pub(super) fn sandbox_for(&self, node_id: &str, mode: Mode) -> Result<Sandbox, String> {
        match self.warm_workspace_template(node_id)? {
            Some(template) => Sandbox::from_template(&template, mode).map_err(|e| e.to_string()),
            None => self.sandbox(mode),
        }
    }

    pub(super) fn record_cache_failure(
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

    pub(super) fn record_unmaterialized_cache_failures(
        &self,
        node_id: &str,
        reason: RunCacheFailureReasonV5,
    ) {
        if let Some(binding) = self.gate_execution.as_ref() {
            for requested in &binding.caches {
                let kind = match requested {
                    review_config::CacheKindSpec::Cargo => CacheKind::Cargo,
                };
                self.record_cache_failure(node_id, kind, reason);
            }
        }
    }

    pub(super) fn reviewer_binding_node(&self, node_id: &str) -> String {
        self.dynamic_reviewer_bases
            .lock()
            .expect("dynamic reviewer bases")
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| node_id.to_string())
    }

    pub(super) fn rebuild_ledger_projection(&self) -> LedgerProjection {
        LedgerProjection::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .expect("replay")
    }

    pub(super) fn with_ledger<R>(&self, inspect: impl FnOnce(&Ledger) -> R) -> R {
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

    pub(super) fn take_ledger_projection(&self) -> LedgerProjection {
        let mut cached = self.ledger_cache.lock().expect("ledger cache");
        if let Some(projection) = cached.take() {
            return projection;
        }
        // See `with_ledger`: keeping this lock closes the same stale-repopulation window.
        self.rebuild_ledger_projection()
    }

    pub(super) fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        self.with_ledger(|ledger| ledger.convergence(policy))
    }

    /// Append one event to the run's log. Everything the kernel decides goes through here:
    /// the log is the authority a run is rebuilt from, so a decision it never saw is a
    /// decision that, on replay, never happened.
    pub(super) fn bind_authority(&self, mut event: NewEvent) -> NewEvent {
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

    pub(super) fn append(&self, event: NewEvent) -> Result<(), String> {
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

    pub(super) fn append_batch(&self, events: &[NewEvent]) -> Result<(), String> {
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

    pub(super) fn fold_appended_into_ledger_cache(&self, events: &[review_core::RunEvent]) {
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

    /// Hold a reviewer-thread event for the canonical-order flush. See `reviewer_events`.
    pub(super) fn buffer_reviewer_event(&self, node_id: &str, event: NewEvent) {
        let mut seq = self.reviewer_event_seq.lock().expect("reviewer event seq");
        let key = (node_id.to_string(), *seq);
        *seq += 1;
        self.reviewer_events
            .lock()
            .expect("reviewer events")
            .push((key, event));
    }

    /// Append every still-buffered node event, sorted by `(node, emission order)`, then clear the
    /// buffer. Ordinary successful nodes flush their own events with their output receipt;
    /// gather and final publication drain leftovers from failed or suppressed paths. This makes
    /// each published node batch internally canonical. It does not reorder already-durable
    /// dispatch/failure events or successful node receipts across concurrent nodes. Idempotent:
    /// a second call on an already-drained buffer is a no-op.
    pub(super) fn flush_reviewer_events(&self) -> Result<(), String> {
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        pending.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<NewEvent> = pending.iter().map(|(_, event)| event.clone()).collect();
        self.append_batch(&events)?;
        pending.clear();
        Ok(())
    }
}

/// One raw Generation algorithm for both legacy execution and the typed Task codec adapter.
pub(crate) fn generation_outputs(
    authority: &RoundAuthority,
    node: &Node,
) -> Result<ArtifactMap, String> {
    let mut outputs = ArtifactMap::new();
    for port in &node.outputs {
        let artifacts = if is_generation_finding_set_output(port) {
            if authority.prior_reduction_finding_set_id == authority.finding_genesis_id {
                Vec::new()
            } else {
                vec![authority.prior_reduction_finding_set_id.clone()]
            }
        } else if is_change_set_port(port) {
            vec![
                authority
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
