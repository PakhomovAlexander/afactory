//! Capability-only entry into the Campaign's existing captured Task. The observations
//! describe this doctor invocation; they never assert the business graph was completed.
use super::*;
use review_core::task::usage::{DecimalU64, DecimalU128};
use review_graph::NodeOutcome;
use review_pipeline::task::campaign_review::host::CampaignReviewTaskHost;
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
use review_pipeline::task::{TaskProviderAdmissionReport, TaskRuntime};
use review_store::store::task::TaskProjection;
use serde_json::{Value, json};

pub(crate) fn run(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &review_source_git::Repo,
    campaign: &str,
) -> Result<i32, String> {
    let session = lifecycle::prepare_session(options, cas, store, repo, campaign)?;
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    let work = {
        let shared = review_store::SharedEventStore::new(&mut *store);
        review_pipeline::task::lease::with_heartbeat_controlled(
            &shared,
            cas,
            &session.lease,
            Some(&cancellation),
            || {
                let captured = &session.captured;
                let host = CampaignReviewTaskHost::new(
                    cas,
                    shared.clone(),
                    &captured.compiler,
                    session.lease.clone(),
                    model_bindings(&captured.plan, &captured.captured, &captured.workers)?,
                )?;
                let authority = CapturedTaskAuthority::for_campaign_review(
                    &captured.compiler,
                    &host,
                    &NoTaskDeveloper,
                );
                let runtime = TaskRuntime::with_store(
                    shared.clone(),
                    cas,
                    session.lease.clone(),
                    &authority,
                    &host,
                )?
                .with_cancellation(&cancellation);
                let report = runtime.execute_provider_admissions()?;
                // Read-only hydration leaves the original connection available for renewal.
                // No transaction is retained across this preparation or eventual output.
                let reader =
                    EventStore::open_read_only(options.resolved_state_dir()?.join("events.sqlite"))
                        .map_err(|e| e.to_string())?;
                let state = reader
                    .task_projection(cas, &captured.revision.task_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("Review Task disappeared")?;
                let now = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|e| e.to_string())?
                        .as_millis(),
                )
                .map_err(|_| "Task clock overflow")?;
                let value = outcome(captured, &state, &report, now)?;
                PreparedDoctor::new(&value, &report, options.json)
            },
        )
    };
    lifecycle::release_then_emit(cas, store, &session.lease, work, |prepared| {
        Ok(prepared.emit())
    })
}

struct PreparedDoctor {
    stdout: String,
    failure: Option<String>,
}

impl PreparedDoctor {
    fn new(
        value: &Value,
        report: &TaskProviderAdmissionReport,
        json: bool,
    ) -> Result<Self, String> {
        use std::fmt::Write as _;
        let mut stdout = String::new();
        if json {
            stdout = serde_json::to_string(value).map_err(|e| e.to_string())?;
            stdout.push('\n');
        } else {
            writeln!(
                stdout,
                "provider doctor: {}",
                if value["ready"] == true {
                    "ready"
                } else {
                    "not ready"
                }
            )
            .expect("String output");
            for node in value["admitted_nodes"].as_array().into_iter().flatten() {
                writeln!(stdout, "  admitted {}", node.as_str().unwrap_or("?"))
                    .expect("String output");
            }
            for admission in value["provider_admissions"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(error) = admission["outcome"]["error"].as_str() {
                    writeln!(
                        stdout,
                        "  {}: {error}",
                        admission["node"].as_str().unwrap_or("?")
                    )
                    .expect("String output");
                }
            }
            writeln!(stdout,
                "  Task {}: {} committed tokens; no Gates or business Workers ran in this invocation",
                value["task"]["task_id"].as_str().unwrap_or("?"),
                value["task"]["committed_tokens"].as_str().unwrap_or("?")
            ).expect("String output");
        }
        let failure = if value["ready"] == true {
            None
        } else {
            let failures: Vec<_> = report
                .outcomes
                .iter()
                .filter_map(|(node, outcome)| match outcome {
                    NodeOutcome::Failed { error, .. } => Some(format!("{node}: {error}")),
                    _ => None,
                })
                .collect();
            Some(if value["task"]["resources_exhausted"] == true {
                "Provider doctor cannot declare this Task ready: original Task resources are exhausted".into()
            } else {
                format!(
                    "Provider admission did not complete: {}",
                    failures.join("; ")
                )
            })
        };
        Ok(Self { stdout, failure })
    }

    fn emit(self) -> i32 {
        print!("{}", self.stdout);
        if let Some(error) = self.failure {
            eprintln!("af provider doctor: {error}");
            // A completed observation already emitted its one typed document. Returning
            // an exit code avoids main adding af/error@1 beside the doctor document.
            1
        } else {
            0
        }
    }
}

fn outcome(
    captured: &CapturedReviewTask,
    state: &TaskProjection,
    report: &TaskProviderAdmissionReport,
    now_unix_ms: u64,
) -> Result<Value, String> {
    if state.revision_id != captured.revision_id
        || state.revision != captured.revision
        || state.plan_id.as_deref() != Some(&captured.plan_id)
    {
        return Err("Provider doctor changed its original Task identity".into());
    }
    let graph = &captured.captured.compilation.graph;
    let mut admitted_bindings = BTreeSet::new();
    let mut admissions = Vec::new();
    for (node, outcome) in &report.outcomes {
        let bindings = match graph.nodes.get(node).map(|node| &node.operator) {
            Some(CompiledOperator::ProviderAdmission { bindings }) => bindings,
            _ => return Err("Provider doctor outcome is not captured admission".into()),
        };
        let binding = captured
            .plan
            .bindings
            .get(
                bindings
                    .first()
                    .ok_or("Provider admission has no binding")?,
            )
            .ok_or("Provider admission lost its captured identity")?;
        let outcome = match outcome {
            NodeOutcome::Completed { outputs } => {
                let (id, published) = state
                    .execution
                    .as_ref()
                    .and_then(|e| e.outputs.get(node))
                    .ok_or("Successful Provider admission has no published Task output")?;
                let ids = published
                    .outputs
                    .get("result")
                    .ok_or("Provider admission has no receipt")?
                    .artifact_ids
                    .clone();
                if outputs.get("result") != Some(&ids) {
                    return Err("Provider doctor changed its selected receipt".into());
                }
                admitted_bindings.extend(bindings.iter().cloned());
                json!({"kind":"completed", "output_id":id, "admission_ids":ids})
            }
            NodeOutcome::Failed { error, .. } => json!({"kind":"failed", "error":error}),
            NodeOutcome::Suppressed { .. } => {
                return Err("Provider-only execution cannot suppress a business node".into());
            }
        };
        admissions.push(json!({"node":node,"bindings":bindings,"execution":binding.execution,"outcome":outcome}));
    }
    let admitted_nodes: Vec<_> = captured
        .captured
        .compilation
        .nodes
        .iter()
        .filter_map(
            |(original, mapping)| match &graph.nodes[&mapping.task_node].operator {
                CompiledOperator::ReviewDomain {
                    operation:
                        ReviewOperation::Reviewer { slot } | ReviewOperation::Scatter { slot },
                    ..
                } if admitted_bindings.contains(slot) => Some(original),
                _ => None,
            },
        )
        .collect();
    let budget = state.execution.as_ref().map(|e| &e.budget);
    let breached = budget.is_some_and(|b| b.breached());
    let expired = now_unix_ms >= state.revision.limits.deadline_unix_ms;
    let resources_exhausted = breached || expired;
    let round = captured.compiler.round();
    let authority = round.authority();
    Ok(json!({
        "schema":"af/provider-doctor@2", "ready":report.ready() && !resources_exhausted,
        "run_id":round.binding().campaign_id,
        "authority":{"authority_snapshot_id":authority.authority_snapshot_id(), "campaign_manifest_id":authority.campaign_manifest_id(),
            "subject_id":authority.subject_id(), "head_snapshot_id":authority.head_snapshot_id(), "round_event_id":authority.round_event_id(),
            "round":authority.round(), "epoch":authority.epoch()},
        "task":{"task_id":state.task_id,"revision_id":state.revision_id,"plan_id":captured.plan_id,"phase":state.phase,
            "limits":state.revision.limits,"committed_tokens":DecimalU128::from(budget.map_or(0, |b| b.committed_tokens())),
            "begun_attempts":DecimalU64::from(budget.map_or(0, |b| b.begun_attempts())),"budget_breached":breached,
            "deadline_expired":expired,"resources_exhausted":resources_exhausted},
        "provider_admissions":admissions,"admitted_nodes":admitted_nodes,
        "gates_run":false,"workers_dispatched":false
    }))
}
