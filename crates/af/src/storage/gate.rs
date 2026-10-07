//! What a finished Task's remote checks left on GitHub, and its removal (ADR-0144): each draft
//! gate pull request its evidence recorded is closed and its two `af-gate/<task-id>/` branches
//! are deleted from the mapping's push target, with the same `gh` and `git` the gate uses, and
//! only while each still equals the evidence: the recorded repository, refs and commits.
//!
//! The result is one `gate_cleanup` record in the Task's log, done or failed with the redacted
//! reason. It never changes the Task's result. A failed one is tried again by the next sweep
//! while the mapping still names the repository, and collection never takes a Task whose
//! cleanup is not done; `[storage] keep_gate_pull_requests` keeps everything open.

use std::path::Path;
use std::time::{Duration, Instant};

use review_core::task::TaskPhaseV1;
use review_core::task::remote_check::{GateCleanupOutcomeV1, TaskGateCleanupV1};
use review_pipeline::task::remote_check::github_pr::{self, GithubPrSettings};
use review_pipeline::task::remote_check::{GithubPrTarget, MAPPING_KNOB, RemoteCheckMapping};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore};
use serde::Serialize;

/// How long one cleanup may take: two GitHub calls and one push per Task.
const CLEANUP_WALL: Duration = Duration::from_secs(120);

/// What a finished Task left: the repository, the commits and the pull requests its evidence
/// recorded.
pub(crate) type Leftovers = github_pr::GateEvidence;

/// One cleanup a sweep tried.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Attempted {
    pub(crate) task_id: String,
    pub(crate) outcome: GateCleanupOutcomeV1,
    pub(crate) summary: String,
}

/// The gate leftovers of a finished Task no done cleanup has removed yet: some remote check
/// pushed its branches or opened its pull request. `None` for any other Task.
pub(crate) fn pending(cas: &Cas, state: &TaskProjection) -> Result<Option<Leftovers>, String> {
    if !matches!(state.phase, TaskPhaseV1::Finished { .. })
        || state
            .gate_cleanups
            .last()
            .is_some_and(|cleanup| cleanup.outcome == GateCleanupOutcomeV1::Done)
    {
        return Ok(None);
    }
    let evidence = crate::task_execution::remote_check_evidence(cas, state)?;
    let pushed: Vec<_> = evidence
        .iter()
        .filter(|evidence| {
            evidence.base_commit.is_some()
                || evidence.head_commit.is_some()
                || evidence.pull_request.is_some()
        })
        .collect();
    let Some(first) = pushed.first() else {
        return Ok(None);
    };
    let mut ours: Vec<_> = pushed
        .iter()
        .filter(|evidence| evidence.github == first.github)
        .collect();
    // The latest observation last: its commits are what the branches were last pushed with.
    ours.sort_by_key(|evidence| evidence.observed_unix_ms);
    let latest =
        |commit: fn(&review_core::task::remote_check::RemoteCheckEvidenceV1) -> &Option<String>| {
            ours.iter()
                .rev()
                .find_map(|evidence| commit(evidence).clone())
        };
    let mut pull_requests: Vec<u64> = ours
        .iter()
        .filter_map(|evidence| evidence.pull_request.as_ref().map(|pull| pull.number))
        .collect();
    pull_requests.sort_unstable();
    pull_requests.dedup();
    Ok(Some(Leftovers {
        github: first.github.clone(),
        base_commit: latest(|evidence| &evidence.base_commit),
        head_commit: latest(|evidence| &evidence.head_commit),
        pull_requests,
    }))
}

/// The push target the mapping names for `github` now, if any.
fn target(github: &str) -> Result<Option<(GithubPrTarget, Option<std::path::PathBuf>)>, String> {
    let Some(path) = crate::task_execution::domain::remote_check_mapping()? else {
        return Ok(None);
    };
    Ok(RemoteCheckMapping::read(&path)?
        .and_then(|mapping| mapping.target_for_github(github).cloned())
        .map(|target| (target, Some(path))))
}

/// Clean up and record one Task's leftovers under `lease`. A mapping that no longer names the
/// repository is a failed cleanup that says so.
fn clean(
    cas: &Cas,
    store: &mut EventStore,
    lease: &TaskLease,
    leftovers: &Leftovers,
) -> Result<TaskGateCleanupV1, String> {
    let task_id = lease.task_id();
    let cleanup = match target(&leftovers.github) {
        Ok(Some((target, mapping))) => github_pr::cleanup(
            task_id,
            &target,
            leftovers,
            mapping.as_deref(),
            &GithubPrSettings::default(),
            Instant::now() + CLEANUP_WALL,
        ),
        Ok(None) => failed(
            task_id,
            leftovers,
            format!(
                "{MAPPING_KNOB} no longer names github:{}; its gate pull requests and \
                 branches were left, and a later sweep removes them once it does",
                leftovers.github
            ),
        ),
        Err(error) => failed(
            task_id,
            leftovers,
            format!("{MAPPING_KNOB} could not be read: {error}"),
        ),
    };
    store
        .record_task_gate_cleanup(cas, lease, cleanup.clone())
        .map_err(|error| error.to_string())?;
    Ok(cleanup)
}

fn failed(task_id: &str, leftovers: &Leftovers, reason: String) -> TaskGateCleanupV1 {
    let mut reason = reason;
    reason.truncate(
        (0..=reason
            .len()
            .min(review_core::task::remote_check::MAX_REMOTE_DIAGNOSTIC_BYTES))
            .rev()
            .find(|end| reason.is_char_boundary(*end))
            .unwrap_or(0),
    );
    TaskGateCleanupV1 {
        schema: review_core::task::remote_check::TASK_GATE_CLEANUP_V1.into(),
        github: leftovers.github.clone(),
        pull_requests: leftovers.pull_requests.clone(),
        branches: TaskGateCleanupV1::branches_of(task_id),
        outcome: GateCleanupOutcomeV1::Failed,
        reason: Some(reason),
    }
}

fn summary(cleanup: &TaskGateCleanupV1) -> String {
    match cleanup.outcome {
        GateCleanupOutcomeV1::Done => format!(
            "done: closed {} pull request{} in {} and deleted {}",
            cleanup.pull_requests.len(),
            if cleanup.pull_requests.len() == 1 {
                ""
            } else {
                "s"
            },
            cleanup.github,
            cleanup.branches.join(" and ")
        ),
        GateCleanupOutcomeV1::Failed => format!(
            "failed: {}",
            cleanup.reason.as_deref().unwrap_or("no reason recorded")
        ),
    }
}

/// Whether the machine keeps gate pull requests and branches (`[storage] keep_gate_pull_requests`).
pub(crate) fn keep_gate_pull_requests() -> bool {
    super::policy().is_ok_and(|policy| policy.keep_gate_pull_requests)
}

/// After a Task finished in this process, still under the lease that finished it. Every
/// failure is a warning: the Task's result is already recorded and stays as it is.
pub(crate) fn after_finish(cas: &Cas, store: &mut EventStore, lease: &TaskLease) {
    if keep_gate_pull_requests() {
        return;
    }
    let outcome = (|| -> Result<Option<TaskGateCleanupV1>, String> {
        let state = store
            .task_projection(cas, lease.task_id())
            .map_err(|error| error.to_string())?
            .ok_or("Unknown Task")?;
        match pending(cas, &state)? {
            Some(leftovers) => clean(cas, store, lease, &leftovers).map(Some),
            None => Ok(None),
        }
    })();
    match outcome {
        Ok(Some(cleanup)) => eprintln!(
            "af: gate cleanup of Task {}: {}",
            lease.task_id(),
            summary(&cleanup)
        ),
        Ok(None) => {}
        Err(error) => eprintln!(
            "af: warning: the gate cleanup of Task {} was not recorded: {error}",
            lease.task_id()
        ),
    }
}

/// The sweep's retry: every finished Task of the Store at `state` whose leftovers are still
/// pending and whose repository the mapping still names is cleaned up under a short lease of
/// its own.
pub(crate) fn retry_in_store(state: &Path) -> Vec<Attempted> {
    let mut attempted = Vec::new();
    // Read-only first: a Store with nothing pending is never opened for writing.
    let pending_tasks = pending_in_store(state);
    if pending_tasks.is_empty() {
        return attempted;
    }
    let opened = (|| -> Result<(Cas, EventStore), String> {
        Ok((
            Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?,
            EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?,
        ))
    })();
    let Ok((cas, mut store)) = opened else {
        return attempted;
    };
    for task_id in pending_tasks {
        if let Some(cleanup) = retry_one(&cas, &mut store, &task_id, false) {
            attempted.push(cleanup);
        }
    }
    attempted
}

/// Whether any finished Task of the Store at `state` still has gate leftovers.
pub(crate) fn has_pending(state: &Path) -> bool {
    !pending_in_store(state).is_empty()
}

/// The finished Tasks of the Store at `state` with gate leftovers no done cleanup removed,
/// read-only, with their leftovers.
fn pending_in_store(state: &Path) -> Vec<String> {
    pending_leftovers(state)
        .into_iter()
        .map(|(task_id, _)| task_id)
        .collect()
}

fn pending_leftovers(state: &Path) -> Vec<(String, Leftovers)> {
    let opened = (|| -> Result<(Cas, EventStore), String> {
        Ok((
            Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?,
            EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?,
        ))
    })();
    let Ok((cas, store)) = opened else {
        return Vec::new();
    };
    let Ok(task_ids) = store.task_ids(&cas) else {
        return Vec::new();
    };
    task_ids
        .into_iter()
        .filter_map(|task_id| {
            let state = store.task_projection(&cas, &task_id).ok().flatten()?;
            let leftovers = pending(&cas, &state).ok().flatten()?;
            Some((task_id, leftovers))
        })
        .collect()
}

/// Before collection takes one Task: its pending leftovers go first, whatever the mapping says,
/// so the record says why if they could not. `Ok` when the Task may be collected — it left
/// nothing, its cleanup is done now, or `keep` (`[storage] keep_gate_pull_requests`) keeps
/// everything — and otherwise why it stays; the next sweep tries again.
pub(crate) fn before_collection(state: &Path, task_id: &str, keep: bool) -> Result<(), String> {
    if keep {
        return Ok(());
    }
    let unknown = |error: String| {
        format!("whether Task `{task_id}` left gate branches cannot be read ({error}); it stays")
    };
    // Read-only first: a Task that left nothing never opens the Store for writing here.
    if !is_pending(state, task_id).map_err(unknown)? {
        return Ok(());
    }
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| unknown(e.to_string()))?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|e| unknown(e.to_string()))?;
    match retry_one(&cas, &mut store, task_id, true) {
        Some(attempted) => {
            eprintln!("af: gate cleanup of Task {task_id}: {}", attempted.summary);
            if attempted.outcome == GateCleanupOutcomeV1::Done {
                Ok(())
            } else {
                Err(stays(task_id, &attempted.summary))
            }
        }
        // Another process may have finished it meanwhile; otherwise its lease is held.
        None if !is_pending(state, task_id).map_err(unknown)? => Ok(()),
        None => Err(stays(
            task_id,
            "its gate cleanup could not be tried now: a writer holds the Task's lease",
        )),
    }
}

/// Whether the finished Task `task_id` of the Store at `state` still has gate leftovers no done
/// cleanup removed, read-only.
fn is_pending(state: &Path, task_id: &str) -> Result<bool, String> {
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    match store
        .task_projection(&cas, task_id)
        .map_err(|e| e.to_string())?
    {
        Some(projection) => Ok(pending(&cas, &projection)?.is_some()),
        None => Ok(false),
    }
}

/// Why collection leaves a Task whose gate cleanup is not done.
pub(crate) fn stays(task_id: &str, why: &str) -> String {
    format!(
        "Task `{task_id}` is not collected before its gate cleanup is done ({why}); the next \
         sweep tries again"
    )
}

fn retry_one(
    cas: &Cas,
    store: &mut EventStore,
    task_id: &str,
    without_target: bool,
) -> Option<Attempted> {
    let state = store.task_projection(cas, task_id).ok().flatten()?;
    let leftovers = pending(cas, &state).ok().flatten()?;
    // A retry needs the mapping to still name the repository; collection records why not.
    if !without_target && !matches!(target(&leftovers.github), Ok(Some(_))) {
        return None;
    }
    let lease = store
        .take_task_lease(
            cas,
            task_id,
            &format!("af-gate-cleanup-{}", std::process::id()),
            CLEANUP_WALL.as_millis() as u64 + 30_000,
        )
        .ok()?;
    let recorded = clean(cas, store, &lease, &leftovers);
    let _ = store.release_task_lease(cas, &lease);
    Some(match recorded {
        Ok(cleanup) => Attempted {
            task_id: task_id.to_string(),
            outcome: cleanup.outcome,
            summary: summary(&cleanup),
        },
        Err(error) => Attempted {
            task_id: task_id.to_string(),
            outcome: GateCleanupOutcomeV1::Failed,
            summary: format!("failed: the cleanup was not recorded: {error}"),
        },
    })
}

/// The gate leftovers finished Tasks of the Store at `state` still have, one line each, for an
/// operator to remove by hand: what `af self uninstall --purge` could not reach.
pub(crate) fn unreached(state: &Path) -> Vec<String> {
    pending_leftovers(state)
        .into_iter()
        .map(|(task_id, leftovers)| {
            let pulls = leftovers
                .pull_requests
                .iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>();
            format!(
                "left on github:{}: {}branches {} (Task {task_id}); remove them with `gh pr close` and `git push <push-url> --delete`",
                leftovers.github,
                if pulls.is_empty() {
                    String::new()
                } else {
                    format!("pull requests {} and ", pulls.join(", "))
                },
                TaskGateCleanupV1::branches_of(&task_id).join(" and "),
            )
        })
        .collect()
}
