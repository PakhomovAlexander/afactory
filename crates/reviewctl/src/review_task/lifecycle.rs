//! CLI sequencing for one captured Review Task. All execution and continuation authority
//! stays in the common Store, compiler and domain host.
use super::*;
use review_core::task::TaskPhaseV1;
use review_core::task::review_compat::{LEGACY_REVIEW_ROUND_V1, LegacyReviewRoundV1};
use review_pipeline::task::host::{CapturedTaskAuthority, NoTaskDeveloper};
use review_store::store::task::{TaskLease, TaskProjection};

pub(crate) fn run(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &review_source_git::Repo,
    campaign: &str,
) -> Result<review_pipeline::RunVerdict, String> {
    let PreparedReviewSession {
        mut captured,
        prepared,
        lease,
        resumed,
    } = prepare_session(options, cas, store, repo, campaign)?;
    progress(options, &prepared, &captured);
    let work = (|| {
        if resumed {
            let latest = store
                .latest_round_started(campaign)
                .map_err(|e| e.to_string())?
                .ok_or("Review Campaign lost its Round")?;
            if latest.event_id != captured.compiler.round().authority().round_event_id() {
                captured = recover_handoff(options, cas, store, &captured, &lease, &latest)?;
            } else if options.restart_round {
                captured = advance(options, cas, store, repo, &captured, &lease)?;
            } else if round_closed(cas, store, &captured)? {
                let previous = execute_current(cas, store, &captured, &lease)?;
                if !previous.continuation_required {
                    return prepare_presentation(options, cas, store, &lease, &captured, &previous);
                }
                captured = advance(options, cas, store, repo, &captured, &lease)?;
            }
        }
        let round = captured.compiler.round().binding();
        crate::run_progress(
            options,
            format_args!("round    {} (epoch {})", round.round, round.epoch),
        );
        let execution = execute_current(cas, store, &captured, &lease)?;
        prepare_presentation(options, cas, store, &lease, &captured, &execution)
    })();
    release_then_emit(cas, store, &lease, work, |prepared| {
        Ok(prepared.emit(options))
    })
}

/// Keep all fallible reads and serialization under renewal, without holding the writer
/// mutex across CAS hydration. The reader uses autocommit, not a retained read transaction.
fn prepare_presentation(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    captured: &CapturedReviewTask,
    execution: &RoundExecution,
) -> Result<presentation::PreparedPresentation, String> {
    let shared = review_store::SharedEventStore::new(store);
    review_pipeline::task::lease::with_heartbeat(&shared, cas, lease, || {
        let reader =
            EventStore::open_read_only(options.resolved_state_dir()?.join("events.sqlite"))
                .map_err(|e| e.to_string())?;
        presentation::prepare(options, cas, &reader, captured, execution)
    })
}

/// Never publish a successful observation while its command still owns a Task lease.
/// Work errors retain their original diagnostic, but release is attempted on every path.
pub(super) fn release_then_emit<T, R>(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    work: Result<T, String>,
    emit: impl FnOnce(T) -> Result<R, String>,
) -> Result<R, String> {
    let released = store
        .release_task_lease(cas, lease)
        .map_err(|e| e.to_string());
    match (work, released) {
        (Ok(value), Ok(_)) => emit(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

pub(super) struct PreparedReviewSession {
    pub(super) captured: CapturedReviewTask,
    pub(super) prepared: crate::authority::PreparedRun,
    pub(super) lease: TaskLease,
    pub(super) resumed: bool,
}

pub(super) fn prepare_session(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &review_source_git::Repo,
    campaign: &str,
) -> Result<PreparedReviewSession, String> {
    if !options.provider_resumes.is_empty() {
        return Err("--resume-provider identifies a legacy Provider operation; common Review resumes its original Task Attempts with the same Campaign and explicit Provider bindings".into());
    }
    let id = task_id(campaign);
    let engine = crate::task_execution::engine(cas)?;
    let existing = store.task_projection(cas, &id).map_err(|e| e.to_string())?;
    let (captured, prepared, lease, resumed) = if let Some(state) = existing {
        let binding = recorded_round(cas, &state.revision)?;
        if binding.campaign_id != campaign {
            return Err("Review Task belongs to another Campaign".into());
        }
        let prepared =
            crate::authority::prepare_recorded_round(options, cas, store, &binding.round_event_id)?;
        if matches!(state.phase, TaskPhaseV1::Finished { .. }) {
            return Err(if options.mode == CampaignMode::Light {
                "This light Campaign already completed its single review Round. Fix the concrete findings, run the deterministic project gate, then stop; do not start another Campaign."
            } else {
                "This Review Task is finished; inspect its recorded result. Starting another Campaign requires an explicit human decision."
            }.into());
        }
        let captured = restore(options, cas, store, &state, &prepared, &engine)?;
        let lease = store
            .take_task_lease(cas, &id, &writer(), 15_000)
            .map_err(|e| e.to_string())?;
        let admitted = store
            .recover_task_attempts(cas, &lease)
            .map_err(|e| e.to_string())
            .and_then(|()| admit_existing(cas, store, &captured, &lease));
        if let Err(error) = admitted {
            let _ = store.release_task_lease(cas, &lease);
            return Err(error);
        }
        (captured, prepared, lease, true)
    } else {
        let prepared = crate::authority::prepare(options, cas, store, repo)?;
        preflight_inputs(options, cas, &prepared, UNCAPPED_ATTEMPT_TOKENS)?;
        let captured = capture_new(options, cas, store, &prepared, &id, engine, now_ms()?)?;
        let lease = open_captured(cas, store, &captured)?;
        (captured, prepared, lease, false)
    };
    Ok(PreparedReviewSession {
        captured,
        prepared,
        lease,
        resumed,
    })
}

fn writer() -> String {
    format!("cli-{}", std::process::id())
}
fn now_ms() -> Result<u64, String> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis(),
    )
    .map_err(|_| "Task clock overflow".into())
}

fn recorded_round(cas: &Cas, revision: &TaskRevisionV1) -> Result<LegacyReviewRoundV1, String> {
    let input = revision
        .inputs
        .get("round")
        .ok_or("Review Task lacks its captured Round")?;
    let [id] = input.artifact_ids.as_slice() else {
        return Err("Review Task has ambiguous Round authority".into());
    };
    let frame = cas.get_artifact(id).map_err(|e| e.to_string())?;
    if input.artifact_type != LEGACY_REVIEW_ROUND_V1
        || frame.artifact_type != LEGACY_REVIEW_ROUND_V1
    {
        return Err("Review Task Round has the wrong contract".into());
    }
    let binding: LegacyReviewRoundV1 =
        serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
    binding.validate()?;
    Ok(binding)
}

fn restore(
    options: &Options,
    cas: &Cas,
    store: &EventStore,
    state: &TaskProjection,
    prepared: &crate::authority::PreparedRun,
    engine: &str,
) -> Result<CapturedReviewTask, String> {
    let compiler = LegacyReviewPlanCompiler::reopen(
        cas,
        CapturedLegacyReviewRound::load_recorded(
            cas,
            store,
            &prepared.run_id,
            prepared.authority.round_event_id(),
        )?,
        engine,
        &state.revision.authority.policy_id,
    )?;
    if options
        .provider_admission
        .as_ref()
        .is_some_and(|cost| cost != compiler.provider_admission())
    {
        return Err("Provider admission override differs from the original captured Review Task; omit both bounds or supply the exact captured pair".into());
    }
    preflight_inputs(
        options,
        cas,
        prepared,
        compiler.resources().uncapped_attempt_tokens,
    )?;
    let (plan, captured, plan_id) = if let Some(id) = &state.plan_id {
        let frame = cas.get_artifact(id).map_err(|e| e.to_string())?;
        if frame.artifact_type != EXECUTION_PLAN_V1 {
            return Err("Review Task plan has the wrong contract".into());
        }
        let plan: ExecutionPlanV1 =
            serde_json::from_value(frame.payload).map_err(|e| e.to_string())?;
        let captured = compiler.recompile(cas, &state.revision, &plan)?;
        (plan, captured, id.clone())
    } else {
        let (plan, captured) = compiler.compile(cas, &state.revision_id)?;
        let id = persist(
            cas,
            &state.task_id,
            EXECUTION_PLAN_V1,
            vec![state.revision_id.clone()],
            &plan,
        )?;
        (plan, captured, id)
    };
    let workers = local_workers(options, &prepared.loaded)?;
    model_bindings(&plan, &captured, &workers)?;
    Ok(CapturedReviewTask {
        compiler,
        revision: state.revision.clone(),
        revision_id: state.revision_id.clone(),
        plan,
        plan_id,
        captured,
        workers,
    })
}

fn admit_existing(
    cas: &Cas,
    store: &mut EventStore,
    captured: &CapturedReviewTask,
    lease: &TaskLease,
) -> Result<(), String> {
    let state = store
        .task_projection(cas, lease.task_id())
        .map_err(|e| e.to_string())?
        .ok_or("Review Task disappeared")?;
    if state.revision_id != captured.revision_id
        || state
            .plan_id
            .as_ref()
            .is_some_and(|id| id != &captured.plan_id)
    {
        return Err("Review Task changed while its captured plan was reopened".into());
    }
    let authority = CapturedTaskAuthority::for_legacy_review(
        &captured.compiler,
        &AdmissionOnly,
        &NoTaskDeveloper,
    );
    if !state.admitted {
        if state.plan_id.is_none() {
            store
                .propose_task_plan(cas, lease, &captured.plan_id, &authority)
                .map_err(|e| e.to_string())?;
        }
        store
            .admit_task_plan(cas, lease, &authority)
            .map_err(|e| e.to_string())?;
    } else if matches!(state.phase, TaskPhaseV1::Waiting { .. }) {
        if now_ms()? >= state.revision.limits.deadline_unix_ms {
            store.resume_task_for_recording(cas, lease, &authority)
        } else {
            store.resume_task(cas, lease, &authority)
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn preflight_inputs(
    options: &Options,
    cas: &Cas,
    prepared: &crate::authority::PreparedRun,
    fallback: u64,
) -> Result<(), String> {
    check_inputs(
        options,
        cas,
        &prepared.loaded,
        &prepared.authority,
        prepared.focus.as_deref(),
        fallback,
    )
}

fn check_inputs(
    options: &Options,
    cas: &Cas,
    loaded: &review_config::Loaded,
    authority: &review_pipeline::RoundAuthority,
    focus: Option<&str>,
    fallback: u64,
) -> Result<(), String> {
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(authority.campaign_manifest_id())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let bytes = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|e| e.to_string())?;
    let definition = review_config::Definition::from_toml(
        std::str::from_utf8(&bytes).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let source = authority.change_set().map_or(
        crate::authority::ChangeSetSource::None,
        crate::authority::ChangeSetSource::Resolved,
    );
    let mut sizes = crate::authority::worker_input_sizes(cas, &definition, loaded, source, focus)?;
    for size in &mut sizes {
        let cap = *size.cap_tokens.get_or_insert(fallback);
        size.fits = size.input_tokens < cap;
        crate::run_progress(
            options,
            format_args!(
                "input    {} {} bytes (about {} tokens of a {cap}-token Attempt cap)",
                size.node, size.input_bytes, size.input_tokens
            ),
        );
    }
    crate::authority::refuse_unfit_inputs(&sizes)
}

fn progress(
    options: &Options,
    prepared: &crate::authority::PreparedRun,
    captured: &CapturedReviewTask,
) {
    let convergence = prepared.convergence;
    crate::run_progress(
        options,
        format_args!(
            "mode     {} ({} clean, {} max Round{})",
            options.mode.as_str(),
            convergence.clean_rounds,
            convergence.max_rounds,
            if convergence.max_rounds == 1 { "" } else { "s" }
        ),
    );
    crate::run_progress(options, format_args!("run      {}", prepared.run_id));
    crate::run_progress(
        options,
        format_args!(
            "task     {} revision {}",
            captured.revision.task_id, captured.revision.revision
        ),
    );
    crate::run_progress(
        options,
        format_args!(
            "timeouts reviewer {}s, checks {}s, git capture {}s (pinned)",
            prepared.timeout.as_secs(),
            prepared.check_timeout.as_secs(),
            prepared.git_timeout.as_secs()
        ),
    );
    crate::run_progress(
        options,
        format_args!(
            "budgets  {} lifetime tokens, {} Attempts, deadline {} (original Task)",
            captured.revision.limits.tokens,
            captured.revision.limits.max_attempts,
            captured.revision.limits.deadline_unix_ms
        ),
    );
}

fn round_closed(
    _cas: &Cas,
    store: &EventStore,
    captured: &CapturedReviewTask,
) -> Result<bool, String> {
    let binding = captured.compiler.round().binding();
    store
        .replay(&binding.campaign_id)
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|e| e.causation_id.as_deref() == Some(&binding.round_event_id))
        .try_fold(false, |closed, e| {
            review_core::run_report_closes_round(e)
                .map(|v| closed || v.unwrap_or(false))
                .map_err(|e| e.to_string())
        })
}

fn successor(
    options: &Options,
    cas: &Cas,
    previous: &CapturedReviewTask,
    round: CapturedLegacyReviewRound,
) -> Result<CapturedReviewTask, String> {
    let compiler = LegacyReviewPlanCompiler::reopen(
        cas,
        round,
        &crate::task_execution::engine(cas)?,
        &previous.revision.authority.policy_id,
    )?;
    let revision =
        compiler.prepare_continuation_revision(cas, &previous.revision_id, &previous.revision)?;
    let mut refs = BTreeSet::from([
        revision.authority.policy_id.clone(),
        revision.provenance.adapter_id.clone(),
        previous.revision_id.clone(),
    ]);
    refs.extend(revision.provenance.input_artifact_ids.iter().cloned());
    refs.extend(
        revision
            .inputs
            .values()
            .flat_map(|p| p.artifact_ids.iter().cloned()),
    );
    let revision_id = persist(
        cas,
        &revision.task_id,
        TASK_REVISION_V1,
        refs.into_iter().collect(),
        &revision,
    )?;
    let (plan, captured) = compiler.compile(cas, &revision_id)?;
    let plan_id = persist(
        cas,
        &revision.task_id,
        EXECUTION_PLAN_V1,
        vec![revision_id.clone()],
        &plan,
    )?;
    let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
        cas.get_json(compiler.round().authority().campaign_manifest_id())
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    check_inputs(
        options,
        cas,
        &captured.loaded,
        compiler.round().authority(),
        manifest.focus.as_deref(),
        compiler.resources().uncapped_attempt_tokens,
    )?;
    let workers = previous.workers.clone();
    model_bindings(&plan, &captured, &workers)?;
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

fn advance(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &review_source_git::Repo,
    previous: &CapturedReviewTask,
    lease: &TaskLease,
) -> Result<CapturedReviewTask, String> {
    use review_pipeline::task::legacy_review::host::LegacyReviewTaskHost;
    use review_store::store::task::review_round_publication::TaskReviewRoundSuccessor;
    let next = {
        let shared = review_store::SharedEventStore::new(store);
        let (host, prepared, preview, next) =
            review_pipeline::task::lease::with_heartbeat(&shared, cas, lease, || {
                let host = LegacyReviewTaskHost::new(
                    cas,
                    shared.clone(),
                    &previous.compiler,
                    lease.clone(),
                    model_bindings(&previous.plan, &previous.captured, &previous.workers)?,
                )?;
                let current = CapturedTaskAuthority::for_legacy_review(
                    &previous.compiler,
                    &host,
                    &NoTaskDeveloper,
                );
                let (permit, history) = {
                    let store = shared.lock().expect("Task Store");
                    let permit = store
                        .prepare_task_review_round_publication(cas, lease, &current)
                        .map_err(|e| e.to_string())?;
                    let history = store
                        .replay(permit.campaign_id())
                        .map_err(|e| e.to_string())?;
                    (permit, history)
                };
                // Git and CAS preparation never hold the Store mutex needed by the heartbeat.
                let prepared =
                    crate::authority::prepare_next_round(options, cas, &history, repo, &permit)?;
                let preview = {
                    let store = shared.lock().expect("Task Store");
                    let fresh = store
                        .prepare_task_review_round_publication(cas, lease, &current)
                        .map_err(|e| e.to_string())?;
                    if fresh.next_review_sequence() != permit.next_review_sequence()
                        || fresh.predecessor() != permit.predecessor()
                    {
                        return Err("Review changed during successor Source capture".into());
                    }
                    store
                        .preview_task_review_round(cas, lease, &fresh, &prepared.events, &current)
                        .map_err(|e| e.to_string())?
                };
                if preview.round_event().event_id
                    != permit
                        .event_id_at(prepared.round_event_offset)
                        .map_err(|e| e.to_string())?
                {
                    return Err(
                        "Prepared Review Round differs from its exact publication preview".into(),
                    );
                }
                let next = successor(
                    options,
                    cas,
                    previous,
                    CapturedLegacyReviewRound::from_prospective(cas, &preview)?,
                )?;
                Ok((host, prepared, preview, next))
            })?;
        // Renewals changed only the Task prefix. Stop the heartbeat, then prove the exact
        // same Review history and successor against a fresh original-budget projection.
        let current =
            CapturedTaskAuthority::for_legacy_review(&previous.compiler, &host, &NoTaskDeveloper);
        let (permit, fresh_preview) = {
            let store = shared.lock().expect("Task Store");
            let permit = store
                .prepare_task_review_round_publication(cas, lease, &current)
                .map_err(|e| e.to_string())?;
            let fresh = store
                .preview_task_review_round(cas, lease, &permit, &prepared.events, &current)
                .map_err(|e| e.to_string())?;
            (permit, fresh)
        };
        if fresh_preview.history() != preview.history() {
            return Err("Review history changed during successor compilation".into());
        }
        let handoff = fresh_preview
            .prepare_handoff(cas, &next.plan_id)
            .map_err(|e| e.to_string())?;
        let handoff_id =
            review_store::store::task::review_handoff::capture_task_review_handoff(cas, &handoff)
                .map_err(|e| e.to_string())?;
        let next_authority =
            CapturedTaskAuthority::for_legacy_review(&next.compiler, &host, &NoTaskDeveloper);
        shared
            .lock()
            .expect("Task Store")
            .publish_task_review_round(
                cas,
                lease,
                &permit,
                &prepared.events,
                &current,
                &TaskReviewRoundSuccessor {
                    handoff_id: &handoff_id,
                    authority: &next_authority,
                },
            )
            .map_err(|e| e.to_string())?;
        shared
            .lock()
            .expect("Task Store")
            .continue_task_review(cas, lease, &handoff_id, &next_authority)
            .map_err(|e| e.to_string())?;
        next
    };
    admit_existing(cas, store, &next, lease)?;
    Ok(next)
}

/// The canonical successor can be durable even if the process stopped before the Task
/// handoff. Reconstruct only that exact adjacent successor; never capture newer Source.
fn recover_handoff(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    previous: &CapturedReviewTask,
    lease: &TaskLease,
    latest: &review_core::RunEvent,
) -> Result<CapturedReviewTask, String> {
    use review_core::task::review_handoff::{
        TaskReviewHandoffEvidenceV1 as Evidence, TaskReviewHandoffV1,
    };
    use review_pipeline::task::legacy_review::host::LegacyReviewTaskHost;
    let next = {
        let shared = review_store::SharedEventStore::new(&mut *store);
        review_pipeline::task::lease::with_heartbeat(&shared, cas, lease, || {
            // Hydration uses a separate autocommit reader so renewal can retain the
            // original writer. The transition below still checks both current prefixes.
            let reader =
                EventStore::open_read_only(options.resolved_state_dir()?.join("events.sqlite"))
                    .map_err(|e| e.to_string())?;
            let old = previous.compiler.round().binding();
            let prepared =
                crate::authority::prepare_recorded_round(options, cas, &reader, &latest.event_id)?;
            preflight_inputs(
                options,
                cas,
                &prepared,
                previous.compiler.resources().uncapped_attempt_tokens,
            )?;
            let next = successor(
                options,
                cas,
                previous,
                CapturedLegacyReviewRound::load_recorded(
                    cas,
                    &reader,
                    &old.campaign_id,
                    &latest.event_id,
                )?,
            )?;
            let new = next.compiler.round().binding();
            let history = reader.replay(&old.campaign_id).map_err(|e| e.to_string())?;
            let state = reader
                .task_projection(cas, lease.task_id())
                .map_err(|e| e.to_string())?
                .ok_or("Review Task disappeared")?;
            let evidence = if old.round == new.round {
                let event = history
                    .iter()
                    .find(|e| {
                        e.event_type == review_core::EventType::RoundInputSupersededV1
                            && e.causation_id.as_deref() == Some(&old.round_event_id)
                            && latest.causation_id.as_deref() == Some(&old.round_event_id)
                    })
                    .ok_or("Recorded Review successor has no exact supersession evidence")?;
                Evidence::SupersededInput {
                    superseded_event_id: event.event_id.clone(),
                }
            } else if let Some(phase) = state
                .execution
                .as_ref()
                .and_then(|e| e.active_review_integration())
                .filter(|p| p.integration_committed_event_id().is_some())
            {
                Evidence::IntegratedRound {
                    report_event_id: phase.phase().closing_report_event_id.clone(),
                    phase_id: phase.phase_id().into(),
                    integration_committed_event_id: phase
                        .integration_committed_event_id()
                        .expect("checked")
                        .into(),
                }
            } else {
                let report = history
                    .iter()
                    .rev()
                    .find(|e| {
                        e.causation_id.as_deref() == Some(&old.round_event_id)
                            && e.event_type == review_core::EventType::RunReportV6
                            && review_core::run_report_closes_round(e)
                                .is_ok_and(|v| v == Some(true))
                    })
                    .ok_or("Recorded Review successor has no exact closed predecessor")?;
                Evidence::ClosedRound {
                    report_event_id: report.event_id.clone(),
                }
            };
            let handoff = TaskReviewHandoffV1 {
                task_id: lease.task_id().into(),
                predecessor_revision_id: previous.revision_id.clone(),
                predecessor_plan_id: previous.plan_id.clone(),
                successor_revision_id: next.revision_id.clone(),
                successor_plan_id: next.plan_id.clone(),
                predecessor_round_id: previous.revision.inputs["round"].artifact_ids[0].clone(),
                successor_round_id: next.revision.inputs["round"].artifact_ids[0].clone(),
                evidence,
            };
            let handoff_id =
                review_store::store::task::review_handoff::capture_task_review_handoff(
                    cas, &handoff,
                )
                .map_err(|e| e.to_string())?;
            let host = LegacyReviewTaskHost::new(
                cas,
                shared.clone(),
                &previous.compiler,
                lease.clone(),
                model_bindings(&previous.plan, &previous.captured, &previous.workers)?,
            )?;
            let authority =
                CapturedTaskAuthority::for_legacy_review(&next.compiler, &host, &NoTaskDeveloper);
            shared
                .lock()
                .expect("Task Store")
                .continue_task_review(cas, lease, &handoff_id, &authority)
                .map_err(|e| e.to_string())?;
            Ok(next)
        })?
    };
    admit_existing(cas, store, &next, lease)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::{
        AcceptanceObligationV1, RequiredOutputV1, TaskAuthorityV1, TaskLimitsV1, TaskProvenanceV1,
        VerificationReserveV1,
    };
    use std::sync::mpsc;
    use std::time::Duration;

    fn leased_task() -> (tempfile::TempDir, Cas, EventStore, TaskLease, TaskLimitsV1) {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let policy = cas.put(b"output lease fixture policy").unwrap();
        let limits = TaskLimitsV1 {
            tokens: 10,
            max_attempts: 1,
            deadline_unix_ms: now_ms().unwrap() + 60_000,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 0,
                wall_ms: 0,
            },
        };
        let revision = TaskRevisionV1 {
            task_id: "output-lease".into(),
            revision: 1,
            previous_revision_id: None,
            kind: "document".into(),
            goal: "Preserve the lease/output boundary".into(),
            inputs: BTreeMap::new(),
            required_outputs: BTreeMap::from([(
                "document".into(),
                RequiredOutputV1 {
                    artifact_type: "af/Document@1".into(),
                    cardinality: review_core::PortCardinality::One,
                },
            )]),
            acceptance: BTreeMap::from([(
                "verified".into(),
                AcceptanceObligationV1 {
                    evidence_type: "af/Receipt@1".into(),
                    verifier_policy: policy.clone(),
                },
            )]),
            provenance: TaskProvenanceV1 {
                adapter_id: policy.clone(),
                input_artifact_ids: vec![],
            },
            authority: TaskAuthorityV1 {
                policy_id: policy.clone(),
                allowed_effects: BTreeSet::new(),
                data_destinations: BTreeSet::new(),
            },
            limits: limits.clone(),
            strategy: "fixed".into(),
            pipeline: None,
            facts: BTreeMap::new(),
        };
        let id = persist(
            &cas,
            &revision.task_id,
            TASK_REVISION_V1,
            vec![policy],
            &revision,
        )
        .unwrap();
        let lease = store.open_task(&cas, &id, "presenter", 15_000).unwrap();
        (dir, cas, store, lease, limits)
    }

    #[test]
    fn blocked_output_does_not_hold_task_writer_lease() {
        let (dir, cas, mut store, lease, limits) = leased_task();
        let (started_tx, started_rx) = mpsc::channel();
        let (unblock_tx, unblock_rx) = mpsc::channel();
        std::thread::scope(|scope| {
            let output_cas = &cas;
            let output_lease = &lease;
            let output = scope.spawn(move || {
                release_then_emit(
                    output_cas,
                    &mut store,
                    output_lease,
                    Ok("prepared output"),
                    |value| {
                        started_tx.send(()).unwrap();
                        // Stand in for a full stdout pipe. Another writer must not have to
                        // wait for this sink or for the original fifteen-second lease.
                        unblock_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        Ok(value)
                    },
                )
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let mut next = EventStore::open(dir.path().join("events.sqlite")).unwrap();
            let taken = next.take_task_lease(&cas, lease.task_id(), "next-command", 15_000);
            unblock_tx.send(()).unwrap();
            let next_lease = taken.expect("the writer must be released before output starts");
            assert_eq!(output.join().unwrap().unwrap(), "prepared output");
            let state = next
                .task_projection(&cas, lease.task_id())
                .unwrap()
                .unwrap();
            assert_eq!(
                state.revision.limits, limits,
                "presentation cannot renew resources"
            );
            next.release_task_lease(&cas, &next_lease).unwrap();
        });
    }

    #[test]
    fn fenced_release_does_not_emit_prepared_success() {
        let (dir, cas, mut store, lease, _) = leased_task();
        store.release_task_lease(&cas, &lease).unwrap();
        let mut next = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let next_lease = next
            .take_task_lease(&cas, lease.task_id(), "next-command", 15_000)
            .unwrap();
        let mut emitted = false;
        let error = release_then_emit(&cas, &mut store, &lease, Ok(()), |()| {
            emitted = true;
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("expired or fenced"), "{error}");
        assert!(!emitted, "success must not precede a failed lease release");
        next.release_task_lease(&cas, &next_lease).unwrap();
    }

    #[test]
    fn preparation_error_still_releases_writer_without_output() {
        let (dir, cas, mut store, lease, _) = leased_task();
        let error = release_then_emit(
            &cas,
            &mut store,
            &lease,
            Err::<(), _>("hydration failed".into()),
            |()| panic!("failed preparation must not emit output"),
        );
        assert_eq!(error, Err::<(), _>("hydration failed".into()));
        let mut next = EventStore::open(dir.path().join("events.sqlite")).unwrap();
        let next_lease = next
            .take_task_lease(&cas, lease.task_id(), "next-command", 15_000)
            .unwrap();
        next.release_task_lease(&cas, &next_lease).unwrap();
    }
}
