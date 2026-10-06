//! `af task recover`, its coordinator acknowledgements and `af task continue` (ADR-0141).
//!
//! Recovery never logs in and never trusts a login. A token-free status that reports no signed-in
//! account returns the private-login handoff state with no paid call; an authenticated status
//! earns one bounded verification probe on the Task's own ledger; only a claim covering every
//! still-required context continues the original Task, through the same runtime as `af task
//! run`. Every document is closed and secret-free: it names Providers, generations, exact
//! accounting and opaque coordinator references, never a URL, code, token or account identity.

use super::*;
use review_core::task::auth_recovery::*;
use review_pipeline::task::recovery::{
    AuthContextHost, AuthRecoveryStep, ModelAuthProbe, verify_and_claim,
};
use review_store::store::task::auth_recovery::AuthRecoveryStatus;

const EXIT_HUMAN_ACTION_REQUIRED: i32 = 3;

/// The machine side after `bind_models` and `validate_plan` passed: every bound Model already
/// re-proved its principal, model, effort and invocation policy against the captured plan.
struct BoundMachine;

impl AuthContextHost for BoundMachine {
    fn current(&self, required: &TaskAuthBindingV1) -> Result<TaskAuthBindingV1, String> {
        Ok(required.clone())
    }
    fn authenticated(&self, _: &TaskAuthBindingV1) -> Result<bool, String> {
        Ok(true)
    }
}

fn store_error(error: review_store::StoreError) -> String {
    error.to_string()
}

/// Join Stage 1's completed private login, then ask each still-unverified context whether a
/// signed-in account answers at all. A logged-out context returns the handoff state now, with
/// no Worker, probe or charge. `Some(code)` ends the command.
pub(super) fn before_binding(
    cas: &Cas,
    store: &mut EventStore,
    projection: &TaskProjection,
    login_ref: Option<&str>,
    json: bool,
) -> Result<Option<i32>, String> {
    let Some((_, suspension)) = projection.auth.active_suspension() else {
        return Ok(None);
    };
    if let Some(login_ref) = login_ref {
        let mut joined = false;
        for (key, requirement) in &suspension.contexts {
            let provider = &requirement.binding.context.provider;
            if crate::providers::auth_handoff::completed_login(provider, login_ref)? {
                store
                    .record_provider_auth_login(key, login_ref)
                    .map_err(store_error)?;
                joined = true;
            }
        }
        if !joined {
            return Err(
                "No completed private login of this Task's Providers has that recovery ID".into(),
            );
        }
    }
    for (key, requirement) in &suspension.contexts {
        let recovery = store.provider_auth_recovery(key).map_err(store_error)?;
        if recovery.status == AuthRecoveryStatus::Verified
            && recovery.generation >= requirement.generation
        {
            continue;
        }
        let context = &requirement.binding.context;
        let signed_in = match crate::providers::task::TaskProviderIdentity::probe_auth(
            &context.provider,
            &context.provider_kind,
        ) {
            Ok(identity) => identity.is_some(),
            // A Provider that is no longer configured, or whose CLI cannot start, is not a
            // login problem: recovery needs an explicit decision.
            Err(_) => {
                return blocked(
                    cas,
                    store,
                    &projection.task_id,
                    TaskAuthBlockV1::BindingChanged,
                    json,
                )
                .map(Some);
            }
        };
        if !signed_in {
            store
                .record_task_auth_outcome(
                    cas,
                    &projection.task_id,
                    TaskAuthOutcomeV1::LoginRequired {},
                )
                .map_err(store_error)?;
            return present(cas, store, &projection.task_id, json).map(Some);
        }
    }
    Ok(None)
}

/// Record why recovery cannot proceed, once for this suspension, and present it.
pub(super) fn blocked(
    cas: &Cas,
    store: &mut EventStore,
    id: &str,
    reason: TaskAuthBlockV1,
    json: bool,
) -> Result<i32, String> {
    store
        .record_task_auth_block(cas, id, reason)
        .map_err(store_error)?;
    present(cas, store, id, json)
}

/// Verify and claim under a renewing lease. The Store is never held across the paid probe.
pub(super) fn verify(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    authority: &dyn review_store::store::task::TaskAuthority,
    models: &BTreeMap<String, TaskModelBinding<'_>>,
) -> Result<AuthRecoveryStep, String> {
    let locked = std::sync::Mutex::new(store);
    let probe = ModelAuthProbe { models };
    let cancellation = std::sync::atomic::AtomicBool::new(false);
    review_pipeline::task::lease::with_heartbeat_controlled(
        &locked,
        cas,
        lease,
        Some(&cancellation),
        || {
            crate::interrupt::forwarding(&cancellation, || {
                verify_and_claim(
                    &locked,
                    cas,
                    lease,
                    authority,
                    &BoundMachine,
                    &probe,
                    Some(&cancellation),
                )
            })
        },
    )
}

fn outcome_name(outcome: &TaskAuthOutcomeV1) -> (&'static str, Option<TaskAuthBlockV1>) {
    match outcome {
        TaskAuthOutcomeV1::Resumed {} => ("resumed", None),
        TaskAuthOutcomeV1::LoginRequired {} => ("login_required", None),
        TaskAuthOutcomeV1::VerificationFailed {} => ("verification_failed", None),
        TaskAuthOutcomeV1::Blocked { reason } => ("blocked", Some(*reason)),
    }
}

fn block_name(reason: TaskAuthBlockV1) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The closed `af/task-auth-recovery@1` document and its exit code.
pub(crate) fn document(
    cas: &Cas,
    store: &EventStore,
    id: &str,
) -> Result<(serde_json::Value, i32), String> {
    let projection = store
        .task_projection(cas, id)
        .map_err(store_error)?
        .ok_or("Unknown Task")?;
    let outcomes = store.task_auth_outcomes(cas, id).map_err(store_error)?;
    let suspension = projection.auth.active_suspension().cloned();
    let latest = suspension.as_ref().and_then(|(suspension_id, _)| {
        outcomes
            .iter()
            .filter(|o| &o.suspension_id == suspension_id)
            .max_by_key(|o| (o.recorded_unix_ms, o.generation, o.sequence))
    });
    let (state, reason) = match (&projection.phase, &suspension) {
        (TaskPhaseV1::Finished { .. }, _) => ("terminal", None),
        (_, Some(_)) => match latest.map(|o| outcome_name(&o.outcome)) {
            Some(("resumed", _)) | None => ("suspended", None),
            Some(other) => other,
        },
        (TaskPhaseV1::Running {}, None) if projection.auth.active_claim().is_some() => {
            ("resumed", None)
        }
        _ => ("not_suspended", None),
    };
    let mut contexts = Vec::new();
    let mut login = Vec::new();
    let keys: BTreeSet<String> = projection
        .auth
        .suspensions
        .last()
        .map(|(_, s)| s.contexts.keys().cloned().collect())
        .unwrap_or_default();
    for key in &keys {
        let recovery = store.provider_auth_recovery(key).map_err(store_error)?;
        let context = recovery
            .context
            .clone()
            .ok_or("Recovery log has no context")?;
        let status = match recovery.status {
            AuthRecoveryStatus::Healthy => "healthy",
            AuthRecoveryStatus::Failed => "failed",
            AuthRecoveryStatus::Verified => "verified",
        };
        if state == "login_required" && recovery.status == AuthRecoveryStatus::Failed {
            login.push(json!({"provider":context.provider,"provider_kind":context.provider_kind}));
        }
        contexts.push(json!({
            "context_key": key,
            "provider": context.provider,
            "provider_kind": context.provider_kind,
            "generation": recovery.generation,
            "status": status,
            "login_recorded": !recovery.login_refs.is_empty(),
        }));
    }
    let notifications: Vec<_> = outcomes
        .iter()
        .filter(|o| o.delivery_ref.is_none())
        .filter_map(|o| {
            let participant = o.participant.as_ref()?;
            let (outcome, reason) = outcome_name(&o.outcome);
            let mut value = json!({
                "context_key": o.context_key,
                "outcome_sequence": o.sequence,
                "generation": o.generation,
                "outcome": outcome,
                "requester_ref": participant.requester_ref,
                "coordinator_ref": participant.coordinator_ref,
            });
            if let Some(reason) = reason {
                value["reason"] = json!(block_name(reason));
            }
            Some(value)
        })
        .collect();
    let undeliverable = outcomes.iter().filter(|o| o.participant.is_none()).count();
    let budget = projection.execution.as_ref().map(|e| &e.budget);
    let mut document = json!({
        "schema": TASK_AUTH_RECOVERY_V1,
        "task_id": id,
        "state": state,
        "contexts": contexts,
        "accounting": {
            "chargeable_tokens": budget.map_or(0, |b| b.committed_tokens()).to_string(),
            "verification_tokens": projection.auth.probe_charge().to_string(),
            "begun_attempts": budget.map_or(0, |b| b.begun_attempts()),
            "limit_tokens": projection.revision.limits.tokens,
            "deadline_unix_ms": projection.revision.limits.deadline_unix_ms,
        },
        "notifications": notifications,
        "undeliverable": undeliverable,
    });
    if let Some(reason) = reason {
        document["reason"] = json!(block_name(reason));
    }
    if !login.is_empty() {
        document["login"] = json!(login);
    }
    if let TaskPhaseV1::Finished { result_id } = &projection.phase {
        let result: TaskResultV1 = artifact(cas, result_id, TASK_RESULT_V1)?;
        document["result"] = json!({
            "result_id": result_id,
            "execution": result.execution,
            "acceptance": result.acceptance,
        });
    }
    if state == "terminal" {
        document["continuation"] = match store.task_successor(id).map_err(store_error)? {
            Some(successor) => json!({"available": false, "successor_task_id": successor}),
            None => json!({"available": store.task_continuation_link(cas, id).is_ok()}),
        };
    }
    let code = match state {
        "suspended" | "login_required" | "verification_failed" | "blocked" => {
            EXIT_HUMAN_ACTION_REQUIRED
        }
        _ => 0,
    };
    document["exit_code"] = json!(code);
    Ok((document, code))
}

pub(super) fn present(cas: &Cas, store: &EventStore, id: &str, json: bool) -> Result<i32, String> {
    let (document, code) = document(cas, store, id)?;
    if json {
        println!("{document}");
        return Ok(code);
    }
    let state = document["state"].as_str().unwrap_or_default();
    let summary = match state {
        "terminal" => "finished; it is never reopened, and continuation needs a linked successor",
        "not_suspended" => "not suspended for Provider authentication",
        "resumed" => "verified and resumed under its original plan and limits",
        "suspended" => "suspended until its Provider authentication is verified",
        "login_required" => "waiting for a private Provider sign-in",
        "verification_failed" => "verification failed for a reason other than sign-in",
        "blocked" => "blocked: recovery needs an explicit decision",
        _ => state,
    };
    println!("Task {id}: {summary}");
    if let Some(reason) = document["reason"].as_str() {
        println!("Reason: {reason}");
    }
    for context in document["contexts"].as_array().into_iter().flatten() {
        println!(
            "Provider {} ({}): generation {}, {}",
            context["provider"].as_str().unwrap_or_default(),
            context["provider_kind"].as_str().unwrap_or_default(),
            context["generation"],
            context["status"].as_str().unwrap_or_default(),
        );
    }
    let accounting = &document["accounting"];
    println!(
        "Charged {} of {} tokens ({} for verification), {} Attempts",
        accounting["chargeable_tokens"].as_str().unwrap_or_default(),
        accounting["limit_tokens"],
        accounting["verification_tokens"]
            .as_str()
            .unwrap_or_default(),
        accounting["begun_attempts"],
    );
    Ok(code)
}

/// Acknowledge one outcome its own coordinator delivered. Repeats are idempotent.
pub(crate) fn acknowledge(
    id: &str,
    inspect: &crate::cli::TaskInspectArgs,
    context_key: &str,
    outcome_sequence: u64,
    coordinator_ref: &str,
    delivery_ref: &str,
) -> Result<i32, String> {
    let (_, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    if !state.join("events.sqlite").is_file() {
        return Err("No common Task Store exists".into());
    }
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let owned = store
        .task_auth_outcomes(&cas, id)
        .map_err(store_error)?
        .iter()
        .any(|o| o.context_key == context_key && o.sequence == outcome_sequence);
    if !owned {
        return Err("That outcome does not belong to this Task".into());
    }
    store
        .acknowledge_task_auth_outcome(context_key, outcome_sequence, coordinator_ref, delivery_ref)
        .map_err(store_error)?;
    present(&cas, &store, id, inspect.json)
}

/// Open an explicitly linked successor of a finished, unsatisfied Task. The predecessor stays
/// immutable; the successor keeps its request, acceptance and authority, carries its exact
/// accounting, fits in what its original limits left, and stops at plan preview: running it
/// needs the same confirmation as any captured plan. A login authorizes none of this.
pub(crate) fn continue_task(
    predecessor: &str,
    successor: &str,
    confirm_result: &str,
    inspect: &crate::cli::TaskInspectArgs,
) -> Result<i32, String> {
    if !review_core::task::is_name(successor) || successor == predecessor {
        return Err("A successor needs its own valid Task ID".into());
    }
    let (_, state) = state_path(&inspect.repo, inspect.state.as_deref())?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    if !state.join("events.sqlite").is_file() {
        return Err("No common Task Store exists".into());
    }
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    if store
        .task_projection(&cas, successor)
        .map_err(store_error)?
        .is_some()
    {
        return Err(format!("Task `{successor}` already exists"));
    }
    let link = store
        .task_continuation_link(&cas, predecessor)
        .map_err(store_error)?;
    if link.predecessor_result_id != confirm_result {
        return Err(
            "Result confirmation differs from the predecessor's recorded result; inspect it again"
                .into(),
        );
    }
    let before = store
        .task_projection(&cas, predecessor)
        .map_err(store_error)?
        .ok_or("Unknown predecessor Task")?;
    let link_id = cas
        .put_artifact(
            TASK_CONTINUATION_V1,
            producer(),
            vec![
                link.predecessor_revision_id.clone(),
                link.predecessor_plan_id.clone(),
                link.predecessor_result_id.clone(),
            ],
            None,
            serde_json::to_value(&link).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?
        .0;
    let mut revision = before.revision.clone();
    revision.task_id = successor.into();
    revision.revision = 1;
    revision.previous_revision_id = None;
    revision.limits = link.remaining_limits()?;
    revision.provenance.input_artifact_ids.push(link_id);
    revision.validate()?;
    let authority: RunAuthority = serde_json::from_value(
        cas.get_json(&revision.authority.policy_id)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let compiler = restore_compiler(&cas, &revision.authority.policy_id, &authority)?;
    let selected = match selection::assess(&cas, &authority, &compiler, revision, None)? {
        selection::SelectionAssessment::Prepared(selection::PreparedSelection::Selected(
            selected,
        )) => selected,
        selection::SelectionAssessment::Prepared(selection::PreparedSelection::Generation {
            ..
        }) => {
            return Err(
                "A continuation reuses a captured Pipeline; this request would need a generated plan"
                    .into(),
            );
        }
        selection::SelectionAssessment::Refused { decision, .. } => {
            let decision = serde_json::to_string(&decision).map_err(|e| e.to_string())?;
            return Err(format!(
                "No Pipeline selected for the continuation: {decision}"
            ));
        }
    };
    let plan_id = planning::persist_plan(&cas, &selected.plan)?;
    let developer = developer::host(&cas, &authority, None);
    let trusted = developer::DecisionAuthority {
        compiler: &selected.compiler,
        developer: developer.as_ref(),
    };
    let lease = store
        .open_task(
            &cas,
            &selected.revision_id,
            &format!("cli-{}-{:016x}", std::process::id(), opening_nonce()),
            15_000,
        )
        .map_err(store_error)?;
    let outcome = store
        .propose_task_plan(&cas, &lease, &plan_id, &trusted)
        .map(|_| ())
        .map_err(store_error);
    release(&cas, &mut store, &lease, outcome)?;
    present_with_format(&cas, &store, successor, inspect.json, true, false, None)
}
