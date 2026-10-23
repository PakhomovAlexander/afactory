//! Installed `af review` frontend for the common Task runtime.
//! Campaign/Subject capture stays in authority; this module owns only CLI lifecycle wiring.

use std::collections::{BTreeMap, BTreeSet};

use review_config::captured_review::ReviewMode;
use review_core::task::plan::{ExecutionPlanV1, WorkerExecutionV1};
use review_core::task::{EXECUTION_PLAN_V1, TASK_REVISION_V1, TaskRevisionV1};
use review_graph::NodeKind;
use review_graph::task::{Address, CompiledOperator, OperatorAttemptCost, ReviewOperation};
use review_pipeline::task::campaign_review::plan::{
    CampaignReviewPlanCompiler, ReviewPlanSettings,
};
use review_pipeline::task::campaign_review::{
    CapturedCampaignReviewRound, CapturedReviewCompilation,
};
use review_pipeline::task::host::TaskModelBinding;
use review_runner::task::WorkerModelAdapter;
use review_store::{Cas, EventStore};

use super::{CampaignMode, Options};

mod doctor;
mod lifecycle;
pub(super) use doctor::run as doctor;
mod presentation;
pub(super) use lifecycle::run;

// The installed fallback matches the existing onboarding Attempt default. It is captured
// only for new, previously uncapped Review Tasks; every declared cap takes precedence.
const UNCAPPED_ATTEMPT_TOKENS: u64 = 300_000;
const PROVIDER_TOKENS: u64 = 32_768;
const PROVIDER_WALL_MS: u64 = 45_000;

/// Only initial capture chooses a default. Replay always reads the captured compiler settings.
pub(super) fn initial_provider_admission(options: &Options) -> OperatorAttemptCost {
    options
        .provider_admission
        .clone()
        .unwrap_or(OperatorAttemptCost {
            tokens: PROVIDER_TOKENS,
            wall_ms: PROVIDER_WALL_MS,
        })
}

#[derive(Clone)]
struct LocalWorkers {
    executions: BTreeMap<String, WorkerExecutionV1>,
    adapters: BTreeMap<String, std::sync::Arc<dyn WorkerModelAdapter>>,
}

fn local_workers(
    options: &Options,
    loaded: &review_config::Loaded,
) -> Result<LocalWorkers, String> {
    // The refusals `af review plan` gives, in the same words.
    super::authority::check_provider_bindings(
        options,
        loaded,
        &options.pipeline.to_string_lossy(),
    )?;
    // First pass, token-free: every captured reviewer's binding passes the static rules, in node
    // order, before any Provider CLI starts (ADR-0147, ADR-0148).
    let mut executions = BTreeMap::new();
    let mut models = Vec::new();
    for (node, command) in loaded.reviewers() {
        let kind = super::packaged_runner(command);
        if !matches!(kind.as_str(), "claude" | "codex") {
            if options.provider_bindings.contains_key(node) {
                return Err(format!(
                    "Command Worker {node} has no Model Provider binding"
                ));
            }
            executions.insert(node.clone(), WorkerExecutionV1::Command {});
            continue;
        }
        let package = loaded
            .packages()
            .get(node)
            .ok_or_else(|| format!("Model Worker {node} requires a captured package"))?;
        let alias = options.provider_bindings.get(node).ok_or_else(|| {
            format!("model-backed Worker `{node}` ({kind}) requires explicit `--provider {node}=PROVIDER_ID`")
        })?;
        let manifest: review_config::lock::PackageManifest = toml::from_str(
            std::str::from_utf8(
                package
                    .file("reviewer.toml")
                    .ok_or("Captured Worker has no manifest")?,
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let settings = review_config::lock::reviewer_runner_settings_from_manifest(&manifest)
            .map_err(|e| e.to_string())?;
        crate::model_check::static_rules(&crate::model_check::Binding {
            owner: format!("Model Worker {node}"),
            label: node,
            provider: Some(alias),
            kind: &kind,
            model: &settings.model,
            effort: &settings.effort,
        })?;
        models.push((node, kind, alias, settings));
    }
    // Second pass: one probe per Provider label, then each reviewer's execution and adapter.
    let mut identities = BTreeMap::new();
    let mut adapters = BTreeMap::new();
    for (node, kind, alias, settings) in models {
        if !identities.contains_key(alias) {
            identities.insert(
                alias.clone(),
                super::providers::task::TaskProviderIdentity::probe(alias, &kind)?,
            );
        }
        let identity = &identities[alias];
        let execution = identity.execution(&settings.model, &settings.effort)?;
        if !matches!(&execution, WorkerExecutionV1::Model { provider_kind, .. } if provider_kind == &kind)
            || settings.backend.to_string() != kind
        {
            return Err("Captured Worker and local Provider families differ".into());
        }
        adapters.insert(
            node.clone(),
            std::sync::Arc::from(identity.adapter(&execution)?),
        );
        executions.insert(node.clone(), execution);
    }
    Ok(LocalWorkers {
        executions,
        adapters,
    })
}

fn public_outputs(loaded: &review_config::Loaded) -> Result<BTreeMap<String, Address>, String> {
    let mut outputs = BTreeMap::new();
    for (index, (node, definition)) in loaded
        .planned()
        .nodes
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Ledger)
        .enumerate()
    {
        for (port_index, port) in definition.outputs.iter().enumerate() {
            outputs.insert(
                format!("ledger{index}_output{port_index}"),
                Address {
                    node: node.clone(),
                    port: port.name.clone(),
                },
            );
        }
    }
    if outputs.is_empty() {
        return Err("Installed Review Task requires the captured Ledger output contract".into());
    }
    Ok(outputs)
}

struct CapturedReviewTask {
    compiler: CampaignReviewPlanCompiler,
    revision: TaskRevisionV1,
    revision_id: String,
    plan: ExecutionPlanV1,
    plan_id: String,
    captured: CapturedReviewCompilation,
    workers: LocalWorkers,
}

fn capture_new(
    options: &Options,
    cas: &Cas,
    store: &EventStore,
    prepared: &super::authority::PreparedRun,
    task_id: &str,
    engine_id: String,
    now_unix_ms: u64,
) -> Result<CapturedReviewTask, String> {
    let round = CapturedCampaignReviewRound::load(
        cas,
        store,
        &prepared.run_id,
        prepared.authority.round_event_id(),
    )?;
    let manifest = serde_json::from_value(
        cas.get_json(prepared.authority.campaign_manifest_id())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let workers = local_workers(options, &prepared.loaded)?;
    let resources = review_config::task::campaign_review::resources::ReviewResourcePolicy {
        uncapped_attempt_tokens: UNCAPPED_ATTEMPT_TOKENS,
    };
    let provider_admission = initial_provider_admission(options);
    let mode = match options.mode {
        CampaignMode::Light => ReviewMode::Light,
        CampaignMode::Heavy => ReviewMode::Heavy,
    };
    let limits = resources.task_limits(
        &prepared.loaded,
        &manifest,
        mode,
        &workers.executions,
        &provider_admission,
        now_unix_ms,
    )?;
    let settings = ReviewPlanSettings {
        mode: options.mode.as_str().into(),
        resources,
        outputs: public_outputs(&prepared.loaded)?,
        executions: workers.executions.clone(),
        provider_admission,
        allowed_effects: BTreeSet::new(),
    };
    let compiler = CampaignReviewPlanCompiler::capture(cas, round, engine_id, settings)?;
    let revision = compiler.prepare_revision(cas, task_id, limits)?;
    let mut refs = BTreeSet::from([
        revision.authority.policy_id.clone(),
        revision.provenance.adapter_id.clone(),
    ]);
    refs.extend(revision.provenance.input_artifact_ids.iter().cloned());
    refs.extend(
        revision
            .inputs
            .values()
            .flat_map(|port| port.artifact_ids.iter().cloned()),
    );
    let revision_id = persist(
        cas,
        task_id,
        TASK_REVISION_V1,
        refs.into_iter().collect(),
        &revision,
    )?;
    let (plan, captured) = compiler.compile(cas, &revision_id)?;
    let plan_id = persist(
        cas,
        task_id,
        EXECUTION_PLAN_V1,
        vec![revision_id.clone()],
        &plan,
    )?;
    Ok(CapturedReviewTask {
        compiler,
        revision,
        revision_id,
        plan,
        plan_id,
        captured,
        workers,
    })
}

fn persist(
    cas: &Cas,
    task_id: &str,
    kind: &str,
    refs: Vec<String>,
    value: &impl serde::Serialize,
) -> Result<String, String> {
    cas.put_artifact(
        kind,
        review_core::Producer::KernelOperation {
            run_id: review_store::store::task::task_run_id(task_id).map_err(|e| e.to_string())?,
            node_id: None,
            operation_id: "installed-review-capture@1".into(),
        },
        refs,
        None,
        serde_json::to_value(value).map_err(|e| e.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|e| e.to_string())
}

fn model_bindings<'a>(
    plan: &ExecutionPlanV1,
    captured: &CapturedReviewCompilation,
    workers: &'a LocalWorkers,
) -> Result<BTreeMap<String, TaskModelBinding<'a>>, String> {
    let mut bindings = BTreeMap::new();
    for (original, mapping) in &captured.compilation.nodes {
        let operator = &captured.compilation.graph.nodes[&mapping.task_node].operator;
        let CompiledOperator::ReviewDomain {
            operation: ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
            ..
        } = operator
        else {
            continue;
        };
        let binding = plan
            .bindings
            .get(slot)
            .ok_or("Compiled Review Worker has no exact binding")?;
        if workers.executions.get(original) != Some(&binding.execution) {
            return Err(format!(
                "Current Provider/account binding for {original} differs from the original Review Task plan"
            ));
        }
        if matches!(binding.execution, WorkerExecutionV1::Model { .. }) {
            let adapter = workers
                .adapters
                .get(original)
                .ok_or("Captured Review Model adapter is absent")?;
            bindings.insert(
                slot.clone(),
                TaskModelBinding {
                    binding: binding.clone(),
                    adapter: adapter.as_ref(),
                },
            );
        }
    }
    Ok(bindings)
}

fn task_id(campaign: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"af.review-task-id/1\0");
    digest.update(campaign.as_bytes());
    format!("review-{}", review_core::hex::encode(&digest.finalize()))
}

/// Every Campaign runs on the common Task runtime. Its first capture may fail after preparation
/// and operator events (a superseded Round input, policy time, Evidence, a waiver) and simply
/// retries. Common evidence whose Task is missing refuses, because it must never fall back to a
/// fresh allowance; a log this release cannot replay refuses with the replay error.
pub(super) fn require_common_campaign(
    cas: &Cas,
    store: &EventStore,
    campaign: &str,
) -> Result<(), String> {
    if store
        .task_projection(cas, &task_id(campaign))
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let events = store.replay(campaign).map_err(|error| error.to_string())?;
    if events.iter().any(|event| {
        matches!(
            event.event_type,
            review_core::EventType::TaskReviewResultSelectedV1
                | review_core::EventType::RunReportV6
        )
    }) {
        return Err("Review has common Task evidence but its original Task is unavailable".into());
    }
    Ok(())
}

struct AdmissionOnly;
impl review_pipeline::task::TaskOperatorHost for AdmissionOnly {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        _attempt: &review_store::store::task::execution::ReservedTaskAttempt,
    ) -> Result<String, String> {
        Err("Installed Review plan admission cannot render Worker context".into())
    }
    fn execute(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _definition: &review_graph::task::CompiledNode,
        _: Option<&review_store::store::task::execution::PreparedTaskAttempt>,
        _cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> review_pipeline::task::TaskWorkOutput {
        review_pipeline::task::TaskWorkOutput {
            notes: Default::default(),
            usage_observation: None,
            usage: None,
            outputs: Err("Installed Review plan admission cannot execute work".into()),
            charged_tokens: Some(0),
            raw_artifact_ids: vec![],
            usage_id: None,
            feedback_id: None,
            unknown_usage_cause: None,
            refused_input: false,
        }
    }
}
impl review_pipeline::task::host::TaskDomain for AdmissionOnly {
    fn validate_context(
        &self,
        _: &Cas,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_store::store::task::execution::ReservedTaskAttempt,
        _: &str,
    ) -> Result<(), String> {
        Err("Installed Review plan admission grants no context authority".into())
    }
    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &review_core::task::execution::TaskInvocationV1,
        _: &review_core::task::execution::TaskOutputV1,
        _definition: &review_graph::task::CompiledNode,
    ) -> Result<(), String> {
        Err("Installed Review plan admission grants no output authority".into())
    }
    fn validate_result(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &review_core::task::TaskResultV1,
    ) -> Result<(), String> {
        Err("Installed Review plan admission grants no acceptance authority".into())
    }
}

fn open_captured(
    cas: &Cas,
    store: &mut EventStore,
    captured: &CapturedReviewTask,
) -> Result<review_store::store::task::TaskLease, String> {
    use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
    let authority = CapturedTaskAuthority::for_campaign_review(
        &captured.compiler,
        &AdmissionOnly,
        &NoTaskDeveloper,
    );
    let lease = store
        .open_task(
            cas,
            &captured.revision_id,
            &format!("cli-{}", std::process::id()),
            crate::task_execution::recovery::lease_ms(),
        )
        .map_err(|e| e.to_string())?;
    let admitted = (|| {
        store
            .propose_task_plan(cas, &lease, &captured.plan_id, &authority)
            .map_err(|e| e.to_string())?;
        store
            .admit_task_plan(cas, &lease, &authority)
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = admitted {
        let _ = store.release_task_lease(cas, &lease);
        return Err(error);
    }
    Ok(lease)
}

struct RoundExecution {
    report: review_graph::RunReport,
    ledger: review_store::Ledger,
    attempts: Vec<review_pipeline::AttemptEvidence>,
    verdict: review_pipeline::RunVerdict,
    continuation_required: bool,
    result: Option<review_core::task::TaskResultV1>,
}

fn execute_current(
    cas: &Cas,
    store: &mut EventStore,
    captured: &CapturedReviewTask,
    lease: &review_store::store::task::TaskLease,
) -> Result<RoundExecution, String> {
    use review_pipeline::task::TaskRuntime;
    use review_pipeline::task::campaign_review::host::CampaignReviewTaskHost;
    use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
    let campaign = &captured.compiler.round().binding().campaign_id;
    // Recorded on the run's `af.run` span only (ADR-0153).
    review_pipeline::telemetry::set_campaign(campaign, captured.compiler.round().binding().round);
    let round = captured.compiler.round().authority().round_event_id();
    let closed = store
        .replay(campaign)
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|event| event.causation_id.as_deref() == Some(round))
        .try_fold(false, |closed, event| {
            review_core::run_report_closes_round(event)
                .map(|value| closed || value.unwrap_or(false))
                .map_err(|e| e.to_string())
        })?;
    let shared = review_store::SharedEventStore::new(store);
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    super::interrupt::forwarding(&cancellation, || {
        review_pipeline::task::lease::with_heartbeat_supervised(
            &shared,
            cas,
            lease,
            review_pipeline::task::lease::HeartbeatControl {
                cancellation: Some(&cancellation),
                writer_lost: None,
                timing: crate::task_execution::recovery::timing(),
            },
            || {
                let host = CampaignReviewTaskHost::new(
                    cas,
                    shared.clone(),
                    &captured.compiler,
                    lease.clone(),
                    model_bindings(&captured.plan, &captured.captured, &captured.workers)?,
                )?
                .with_cache_source_resolver(super::caches::resolve_kind);
                // Only a pinned policy that keeps a Warm Workspace needs the cache root; a cold
                // pipeline neither validates nor touches the machine's cache configuration.
                let host = if super::keeps_warm_workspace(&captured.captured.loaded) {
                    host.with_workspace_cache_root(
                        super::config::cache_home()?.join("af").join("workspaces"),
                    )
                } else {
                    host
                };
                let authority = CapturedTaskAuthority::for_campaign_review(
                    &captured.compiler,
                    &host,
                    &NoTaskDeveloper,
                );
                if !closed {
                    let runtime = TaskRuntime::with_store(
                        shared.clone(),
                        cas,
                        lease.clone(),
                        &authority,
                        &host,
                    )?
                    .with_cancellation(&cancellation)
                    .with_lease_timing(crate::task_execution::recovery::timing());
                    runtime.execute()?;
                }
                // An interrupted Round publishes no conclusion; the Campaign resumes (ADR-0129).
                super::interrupt::check()?;
                let conclusion = host.publish_recorded_round_conclusion(cas)?;
                let mut continuation_required = conclusion.can_continue;
                if let Some(mut phase) = host.select_recorded_integration(cas)? {
                    if phase.requires_checks() && !phase.finished() {
                        let runtime = TaskRuntime::with_review_integration(
                            shared.clone(),
                            cas,
                            lease.clone(),
                            &authority,
                            &host,
                            &phase,
                        )?
                        .with_cancellation(&cancellation)
                        .with_lease_timing(crate::task_execution::recovery::timing());
                        let (report_id, _) = runtime.execute_review_integration(&phase)?;
                        super::interrupt::check()?;
                        phase = host.finish_recorded_integration(cas, &phase, &report_id)?;
                    }
                    continuation_required |= phase.integration_committed_event_id().is_some();
                }
                let result = if !continuation_required
                    && !matches!(
                        conclusion.verdict,
                        review_pipeline::RunVerdict::Incomplete { .. }
                    ) {
                    let result = host.assemble_recorded_result(cas)?;
                    let mut refs = result.evidence.clone();
                    refs.insert(result.task_revision_id.clone());
                    refs.extend(
                        result
                            .outputs
                            .values()
                            .flat_map(|port| port.artifact_ids.iter().cloned()),
                    );
                    let id = persist(
                        cas,
                        lease.task_id(),
                        review_core::task::TASK_RESULT_V1,
                        refs.into_iter().collect(),
                        &result,
                    )?;
                    {
                        let mut store = shared.lock().expect("Task Store");
                        store
                            .finish_task(cas, lease, &id, &authority)
                            .map_err(|e| e.to_string())?;
                        review_pipeline::telemetry::finished(&store, lease);
                    }
                    Some(result)
                } else {
                    None
                };
                Ok(RoundExecution {
                    report: conclusion.report,
                    ledger: host.ledger(),
                    attempts: host.selected_attempt_evidence()?,
                    verdict: conclusion.verdict,
                    continuation_required,
                    result,
                })
            },
        )
    })
}
