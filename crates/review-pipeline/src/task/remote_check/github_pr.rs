//! The `github-pr` executor (ADR-0140 §3.4): two `af-gate/<task-id>/` branches built from
//! Snapshots, one draft pull request between them, and a bounded wait on the declared
//! workflow's `pull_request` run for that pull request and head commit.
//!
//! The executor never force-pushes, writes no ref outside this Task's two branches, and never
//! merges, marks ready, closes, comments on or deletes anything. For a required job that did not
//! succeed it keeps a bounded tail of the job's log, so a remote failure can be debugged where a
//! local one is.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use review_core::task::remote_check::{
    MAX_REMOTE_NAME_CHARS, MAX_REMOTE_STEPS, RemoteCheckReasonV1, RemoteCheckStateV1, RemoteJobV1,
    RemotePullRequestV1, RemoteRunV1, RemoteStepV1,
};
use review_source_git::Manifest;
use review_store::Cas;
use serde_json::Value;

use super::gate::{
    GateRepository, GateRole, ToolError, Tools, gate_message, is_ref_component, push_refspec,
};
use super::{EvidenceBase, GithubPrTarget, Redactor, RemoteCheckOutcome, RemoteCheckRequest};

/// How the executor reaches `git` and `gh` and how it paces its wait. The defaults are the
/// design's; a test shortens them.
#[derive(Debug, Clone)]
pub struct GithubPrSettings {
    /// The PATH `git` and `gh` are found on; the coordinator's own when `None`.
    pub path: Option<OsString>,
    /// Between two observations of the remote.
    pub poll_interval: Duration,
    /// How long after the push zero runs of a declared workflow means it does not run.
    pub missing_after: Duration,
}

impl Default for GithubPrSettings {
    fn default() -> Self {
        Self {
            path: None,
            poll_interval: Duration::from_secs(15),
            missing_after: Duration::from_secs(600),
        }
    }
}

/// Everything one remote phase needs: the exact Snapshots, the operator's target, the check
/// node's remote checks, the phase's deadline and the Attempt's cancellation flag.
pub struct RemotePhase<'a> {
    pub cas: &'a Cas,
    pub task_id: &'a str,
    /// The Task's durable identity in its Store, named by every gate commit.
    pub owner: &'a str,
    pub candidate_id: &'a str,
    pub candidate: &'a Manifest,
    /// The Task's captured source: the candidate's root ancestor.
    pub source_id: &'a str,
    pub source: &'a Manifest,
    pub target: &'a GithubPrTarget,
    /// The mapping's path, redacted from every kept diagnostic.
    pub mapping: Option<&'a Path>,
    pub checks: &'a [RemoteCheckRequest],
    pub deadline: Instant,
    pub cancellation: Option<&'a AtomicBool>,
}

const PR_TITLE_PREFIX: &str = "af gate: ";
const PR_BODY: &str = "Opened by af on an operator's machine to run this repository's declared \
checks against an exact Task Snapshot (ADR-0140). It is not for review or merge: af never marks \
it ready, merges, closes or comments on it. Close it and delete its two af-gate branches when the \
Task no longer needs them.";
const WAIT_SLICE: Duration = Duration::from_millis(50);
const MAX_PAGES: u32 = 10;
/// The bound of the one read-back that follows an interrupted push.
const PUSH_RECOVERY: Duration = Duration::from_secs(20);
/// The tail kept of one unsuccessful job's log.
pub const MAX_JOB_LOG_BYTES: usize = 256 * 1024;
/// The log excerpt kept for one check, across its unsuccessful jobs.
pub const MAX_CHECK_LOG_BYTES: usize = 1024 * 1024;

/// Run the remote phase once for every remote check of one Check node. `Err` is a
/// kernel error (a tree that did not read back, an unwritable private repository), never a
/// check result; every remote refusal and inconclusive state is an outcome.
pub fn run(
    phase: &RemotePhase<'_>,
    settings: &GithubPrSettings,
) -> Result<Vec<RemoteCheckOutcome>, String> {
    let base = EvidenceBase {
        github: &phase.target.github,
        snapshot_id: phase.candidate_id,
        source_snapshot_id: phase.source_id,
    };
    let refuse_all = |reason: RemoteCheckReasonV1, message: String, diagnostic: Option<String>| {
        phase
            .checks
            .iter()
            .map(|check| {
                base.refused(
                    check,
                    reason,
                    format!("remote check `{}`: {message}", check.name),
                    diagnostic.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    if let Some(path) = ci_difference(phase.source, phase.candidate) {
        return Ok(refuse_all(
            RemoteCheckReasonV1::RemoteCandidateChangesCi,
            format!(
                "the candidate differs from the Task's source Snapshot under `.github/` (first \
                 at `{path}`), so it is never sent to a remote executor; run this Task where the \
                 check is local, with a pipeline whose check node lists it in `checks` instead \
                 of `remote_checks`"
            ),
            None,
        ));
    }
    if !is_ref_component(phase.task_id) {
        return Ok(refuse_all(
            RemoteCheckReasonV1::RemoteRefInvalid,
            format!(
                "Task ID {:?} is not a single Git ref component, so it cannot name \
                 `af-gate/<task-id>/` branches; use a ref-safe Task ID",
                phase.task_id
            ),
            None,
        ));
    }
    let mut redactor = Redactor::new(&phase.target.push_url, phase.mapping);
    let directory = tempfile::tempdir()
        .map_err(|error| format!("creating the private gate repository: {}", error.kind()))?;
    redactor.add_path(directory.path(), "<gate-repository>");
    let tools = Tools {
        path: settings.path.as_deref(),
        deadline: phase.deadline,
        cancellation: phase.cancellation,
        redactor: &redactor,
    };
    // An interrupted phase before anything was published is a refusal for every check.
    let interrupted = |error: &ToolError| match error {
        ToolError::Cancelled => Some(refuse_all(
            RemoteCheckReasonV1::Cancelled,
            "the Attempt was cancelled before anything was published".into(),
            None,
        )),
        ToolError::TimedOut => Some(refuse_all(
            RemoteCheckReasonV1::DeadlineExpired,
            "the remote phase ran out of time (check_process_wall_ms or the check Attempt's \
             deadline) before anything was published; raise the limit or run the check locally"
                .into(),
            None,
        )),
        _ => None,
    };
    let unavailable = |program: &str, fix: &str, diagnostic: Option<String>| {
        refuse_all(
            RemoteCheckReasonV1::RemoteToolUnavailable,
            format!("`{program}` is not usable on this machine; {fix}"),
            diagnostic,
        )
    };
    match tools.run("git", &["--version"], None, true, directory.path(), &[]) {
        Ok(_) => {}
        Err(error) => {
            if let Some(outcomes) = interrupted(&error) {
                return Ok(outcomes);
            }
            let diagnostic = match error {
                ToolError::Failed(diagnostic) => diagnostic,
                _ => None,
            };
            return Ok(unavailable(
                "git",
                "install Git and put it on PATH",
                diagnostic,
            ));
        }
    }
    // `gh auth status` may print account details: its output is never kept.
    match tools.run(
        "gh",
        &["auth", "status", "--hostname", "github.com"],
        None,
        false,
        directory.path(),
        &[],
    ) {
        Ok(_) => {}
        Err(ToolError::Missing(_)) => {
            return Ok(unavailable(
                "gh",
                "install the GitHub CLI and put it on PATH",
                None,
            ));
        }
        Err(error) => {
            if let Some(outcomes) = interrupted(&error) {
                return Ok(outcomes);
            }
            return Ok(unavailable(
                "gh",
                "it is not authenticated for github.com; run `gh auth login`",
                None,
            ));
        }
    }

    let kernel = |what: &str, error: Result<ToolError, String>| match error {
        Ok(error) => redactor.apply(&error.describe(what)),
        Err(message) => redactor.apply(&format!("{what}: {message}")),
    };
    let repository = match GateRepository::init(&tools, directory) {
        Ok(repository) => repository,
        Err(error) => {
            if let Some(outcomes) = interrupted(&error) {
                return Ok(outcomes);
            }
            return Err(kernel(
                "initializing the private gate repository",
                Ok(error),
            ));
        }
    };
    let trees = match repository.write_trees(&tools, phase.cas, &[phase.source, phase.candidate]) {
        Ok(trees) => trees,
        Err(Ok(error)) if interrupted(&error).is_some() => {
            return Ok(interrupted(&error).unwrap_or_default());
        }
        Err(error) => return Err(kernel("writing Snapshot trees", error)),
    };
    let (source_tree, candidate_tree) = (trees[0].clone(), trees[1].clone());
    let base_message = gate_message(GateRole::Base, phase.task_id, phase.owner, phase.source_id);
    let head_message = gate_message(
        GateRole::Head,
        phase.task_id,
        phase.owner,
        phase.candidate_id,
    );
    let base_commit = match repository.commit(&tools, &source_tree, None, &base_message) {
        Ok(commit) => commit,
        Err(Ok(error)) if interrupted(&error).is_some() => {
            return Ok(interrupted(&error).unwrap_or_default());
        }
        Err(error) => return Err(kernel("writing the base gate commit", error)),
    };
    let base_branch = format!("af-gate/{}/base", phase.task_id);
    let head_branch = format!("af-gate/{}/head", phase.task_id);
    let base_ref = format!("refs/heads/{base_branch}");
    let head_ref = format!("refs/heads/{head_branch}");
    let url = phase.target.push_url.as_str();
    let delete_hint = format!(
        "another Task, or this Task ID in another Store, owns the name; delete both branches \
         (`git push <push-url> --delete {base_branch} {head_branch}`) or rename the Task"
    );

    let found = match repository.ls_remote(&tools, url, &[&base_ref, &head_ref]) {
        Ok(found) => found,
        Err(error) => {
            if let Some(outcomes) = interrupted(&error) {
                return Ok(outcomes);
            }
            let diagnostic = match error {
                ToolError::Failed(diagnostic) => diagnostic,
                ToolError::Missing(_) => {
                    return Ok(unavailable("git", "install Git and put it on PATH", None));
                }
                _ => None,
            };
            return Ok(refuse_all(
                RemoteCheckReasonV1::RemotePushRefused,
                "the push remote refused to list the gate branches; see the diagnostic \
                 (permission, or a wrong `push_url` in the mapping)"
                    .into(),
                diagnostic,
            ));
        }
    };
    if let Some(existing) = found.get(&base_ref)
        && *existing != base_commit
    {
        return Ok(refuse_all(
            RemoteCheckReasonV1::RemoteRefConflict,
            format!(
                "branch `{base_branch}` already exists with another commit than this Task's \
                 base; {delete_hint}"
            ),
            None,
        ));
    }
    let mut refspecs = Vec::new();
    if !found.contains_key(&base_ref) {
        refspecs.push(push_refspec(&base_commit, &base_ref, phase.task_id)?);
    }
    let head_commit = if found.contains_key(&head_ref) {
        let tip = match repository.fetch(&tools, url, &head_ref) {
            Ok(Some(tip)) => tip,
            Ok(None) => {
                return Ok(refuse_all(
                    RemoteCheckReasonV1::RemoteRefConflict,
                    format!("branch `{head_branch}` changed while it was read; {delete_hint}"),
                    None,
                ));
            }
            Err(error) => {
                if let Some(outcomes) = interrupted(&error) {
                    return Ok(outcomes);
                }
                let diagnostic = match error {
                    ToolError::Failed(diagnostic) => diagnostic,
                    _ => None,
                };
                return Ok(refuse_all(
                    RemoteCheckReasonV1::RemotePushRefused,
                    "the push remote refused to serve the head gate branch; see the diagnostic"
                        .into(),
                    diagnostic,
                ));
            }
        };
        let chain = match repository.verify_chain(
            &tools,
            &tip,
            &base_commit,
            phase.task_id,
            phase.owner,
        ) {
            Ok(Ok(chain)) => chain,
            Ok(Err(why)) => {
                return Ok(refuse_all(
                    RemoteCheckReasonV1::RemoteRefConflict,
                    format!(
                        "branch `{head_branch}` is not this Task's chain of gate commits on its \
                         base ({why}); {delete_hint}"
                    ),
                    None,
                ));
            }
            Err(error) => {
                if let Some(outcomes) = interrupted(&error) {
                    return Ok(outcomes);
                }
                return Err(kernel("reading the head gate branch", Ok(error)));
            }
        };
        // A gate-shaped message is not lineage. Every head commit must name a Snapshot this
        // Task's Store holds and carry exactly that Snapshot's tree, so a commit someone else
        // appended under this Task's name is refused, whatever tree it has.
        let mut named = Vec::new();
        for commit in &chain {
            match review_source_git::task::read_snapshot(phase.cas, &commit.snapshot) {
                Ok((_, manifest)) => named.push(manifest),
                Err(_) => {
                    return Ok(refuse_all(
                        RemoteCheckReasonV1::RemoteRefConflict,
                        format!(
                            "commit {} on branch `{head_branch}` names a Snapshot this Task's \
                             Store does not hold; {delete_hint}",
                            commit.id
                        ),
                        None,
                    ));
                }
            }
        }
        let trees =
            match repository.write_trees(&tools, phase.cas, &named.iter().collect::<Vec<_>>()) {
                Ok(trees) => trees,
                Err(Ok(error)) if interrupted(&error).is_some() => {
                    return Ok(interrupted(&error).unwrap_or_default());
                }
                Err(error) => {
                    return Err(kernel("writing the head branch's Snapshot trees", error));
                }
            };
        if let Some((commit, _)) = chain
            .iter()
            .zip(&trees)
            .find(|(commit, tree)| commit.tree != **tree)
        {
            return Ok(refuse_all(
                RemoteCheckReasonV1::RemoteRefConflict,
                format!(
                    "commit {} on branch `{head_branch}` does not carry the tree of the Snapshot \
                     it names; {delete_hint}",
                    commit.id
                ),
                None,
            ));
        }
        // Only a tip made for this very candidate is attached to.
        let tip_commit = &chain[0];
        if tip_commit.tree == candidate_tree && tip_commit.snapshot == phase.candidate_id {
            tip
        } else {
            let commit = match repository.commit(&tools, &candidate_tree, Some(&tip), &head_message)
            {
                Ok(commit) => commit,
                Err(Ok(error)) if interrupted(&error).is_some() => {
                    return Ok(interrupted(&error).unwrap_or_default());
                }
                Err(error) => return Err(kernel("writing the head gate commit", error)),
            };
            refspecs.push(push_refspec(&commit, &head_ref, phase.task_id)?);
            commit
        }
    } else {
        let commit =
            match repository.commit(&tools, &candidate_tree, Some(&base_commit), &head_message) {
                Ok(commit) => commit,
                Err(Ok(error)) if interrupted(&error).is_some() => {
                    return Ok(interrupted(&error).unwrap_or_default());
                }
                Err(error) => return Err(kernel("writing the head gate commit", error)),
            };
        refspecs.push(push_refspec(&commit, &head_ref, phase.task_id)?);
        commit
    };
    let published = Published {
        base: &base,
        base_commit: &base_commit,
        head_commit: &head_commit,
        tree: &candidate_tree,
    };
    let mut pushed_at = Instant::now();
    if !refspecs.is_empty() {
        let pushed = match repository.push(&tools, url, &refspecs) {
            // The remote may have accepted the atomic update before the client was stopped, so
            // an interrupted push proves nothing either way. Read the two branches back through
            // a recovery call with its own short bound, which runs although the phase's
            // deadline has passed or the Attempt is cancelled; it reads, it never judges.
            Err(error @ (ToolError::TimedOut | ToolError::Cancelled)) => {
                let recovery = Tools {
                    path: settings.path.as_deref(),
                    deadline: Instant::now() + PUSH_RECOVERY,
                    cancellation: None,
                    redactor: &redactor,
                };
                match repository.ls_remote(&recovery, url, &[&base_ref, &head_ref]) {
                    // Both branches are as pushed: the push landed. With time left the phase
                    // goes on; otherwise the branches are recorded as published, never refused.
                    Ok(now)
                        if now.get(&base_ref) == Some(&base_commit)
                            && now.get(&head_ref) == Some(&head_commit) =>
                    {
                        if tools.cancelled() || tools.remaining().is_zero() {
                            let (reason, message) = match error {
                                ToolError::Cancelled => (
                                    RemoteCheckReasonV1::Cancelled,
                                    "the Attempt was cancelled as the gate branches were pushed; \
                                     the push landed: resume the Task to attach to them",
                                ),
                                _ => (
                                    RemoteCheckReasonV1::DeadlineExpired,
                                    "the remote phase ran out of time (check_process_wall_ms or \
                                     the check Attempt's deadline) as the gate branches were \
                                     pushed; the push landed: resume the Task to attach to them, \
                                     or raise the limit",
                                ),
                            };
                            return Ok(phase
                                .checks
                                .iter()
                                .map(|check| {
                                    published.outcome(check, reason, message.into(), None, None)
                                })
                                .collect());
                        }
                        Ok(())
                    }
                    // Both branches are as found before the push: nothing landed.
                    Ok(now) if now == found => {
                        return Ok(interrupted(&error).unwrap_or_default());
                    }
                    // Unreadable, or half of an update that is atomic: no claim is recorded.
                    _ => {
                        return Err(format!(
                            "the push of the gate branches `{base_branch}` and `{head_branch}` \
                             was interrupted and the remote's state could not be read back; \
                             nothing is recorded about it: resume the Task to reconcile the \
                             gate branches"
                        ));
                    }
                }
            }
            other => other,
        };
        if let Err(error) = pushed {
            let diagnostic = match error {
                ToolError::Failed(diagnostic) => diagnostic,
                _ => None,
            };
            return Ok(refuse_all(
                RemoteCheckReasonV1::RemotePushRefused,
                "the push remote rejected the gate branches; the diagnostic names why (a \
                 ruleset or a permission)"
                    .into(),
                diagnostic,
            ));
        }
        pushed_at = Instant::now();
    }

    let github = phase.target.github.as_str();
    let api = Api {
        tools: &tools,
        github,
        cwd: repository.root(),
    };
    let pull = match api.find_pull(&head_branch, &base_branch) {
        Ok(FoundPull::Open(pull)) => pull,
        // A gate pull request someone closed is never replaced by a second one.
        Ok(FoundPull::Closed(closed)) => {
            let pull_request = RemotePullRequestV1 {
                number: closed.number,
                url: closed.url.clone(),
            };
            return Ok(phase
                .checks
                .iter()
                .map(|check| {
                    published.outcome(
                        check,
                        RemoteCheckReasonV1::RemotePrRefused,
                        format!(
                            "this Task's gate pull request #{} is closed, and af opens only one; \
                             reopen it (`gh pr reopen {}`), or delete both gate branches to \
                             start over, then resume the Task",
                            closed.number, closed.number
                        ),
                        None,
                        Some(&pull_request),
                    )
                })
                .collect());
        }
        Ok(FoundPull::None) => match api.create_pull(phase.task_id, &head_branch, &base_branch) {
            Ok(pull) => pull,
            Err(error) => {
                return Ok(phase
                    .checks
                    .iter()
                    .map(|check| published.pull_failure(check, None, &error))
                    .collect());
            }
        },
        Err(error) => {
            return Ok(phase
                .checks
                .iter()
                .map(|check| published.pull_failure(check, None, &error))
                .collect());
        }
    };
    let pull_request = RemotePullRequestV1 {
        number: pull.number,
        url: pull.url.clone(),
    };

    // Wait: observe immediately, then every poll interval, until every check is decided.
    let mut decided: BTreeMap<String, RemoteCheckOutcome> = BTreeMap::new();
    let mut stale: Option<String> = None;
    let mut last_diagnostic: Option<String> = None;
    loop {
        let undecided: Vec<&RemoteCheckRequest> = phase
            .checks
            .iter()
            .filter(|check| !decided.contains_key(&check.name))
            .collect();
        if undecided.is_empty() {
            break;
        }
        if tools.cancelled() {
            for check in undecided {
                decided.insert(
                    check.name.clone(),
                    published.outcome(
                        check,
                        RemoteCheckReasonV1::Cancelled,
                        "the Attempt was cancelled while the remote check ran; resume the Task \
                         to attach to the same pull request"
                            .into(),
                        None,
                        Some(&pull_request),
                    ),
                );
            }
            break;
        }
        if tools.remaining().is_zero() {
            for check in undecided {
                let (reason, message) = match &stale {
                    Some(why) => (
                        RemoteCheckReasonV1::RemoteMergeMismatch,
                        format!(
                            "pull request #{} or its merge ref is not what af pushed ({why}); \
                             someone changed the gate pull request: restore or delete it",
                            pull.number
                        ),
                    ),
                    None => (
                        RemoteCheckReasonV1::DeadlineExpired,
                        format!(
                            "the required jobs of `{}` did not complete within the remote phase \
                             (check_process_wall_ms or the check Attempt's deadline); resume the \
                             Task to attach to pull request #{} again, or raise the limit",
                            check.declaration.workflow, pull.number
                        ),
                    ),
                };
                decided.insert(
                    check.name.clone(),
                    published.outcome(
                        check,
                        reason,
                        message,
                        last_diagnostic.clone(),
                        Some(&pull_request),
                    ),
                );
            }
            break;
        }
        let mut ready: Vec<(&RemoteCheckRequest, Run, Vec<Job>)> = Vec::new();
        match api.runs(&head_commit) {
            Ok(runs) => {
                let mut jobs_of: BTreeMap<(u64, u64), Result<Vec<Job>, ToolError>> =
                    BTreeMap::new();
                for check in &undecided {
                    let kept: Vec<&Run> = runs
                        .iter()
                        .filter(|run| {
                            run.event == "pull_request"
                                && run.head_sha == head_commit
                                && run.path == check.declaration.workflow
                                && run.pull_requests.contains(&pull.number)
                        })
                        .collect();
                    match kept.as_slice() {
                        [] => {
                            if pushed_at.elapsed() >= settings.missing_after {
                                decided.insert(
                                    check.name.clone(),
                                    published.outcome(
                                        check,
                                        RemoteCheckReasonV1::RemoteCheckMissing,
                                        format!(
                                            "no `pull_request` run of `{}` exists for pull \
                                             request #{} at head commit {head_commit} {} seconds \
                                             after the push; correct the workflow path in \
                                             [checks.{}.remote] or its trigger (it must run for \
                                             draft pull requests into `af-gate/**` bases)",
                                            check.declaration.workflow,
                                            pull.number,
                                            settings.missing_after.as_secs(),
                                            check.name
                                        ),
                                        None,
                                        Some(&pull_request),
                                    ),
                                );
                            }
                        }
                        [run] => {
                            let jobs = jobs_of
                                .entry((run.id, run.attempt))
                                .or_insert_with(|| api.jobs(run.id, run.attempt));
                            let jobs = match jobs {
                                Ok(jobs) => jobs,
                                Err(ToolError::Failed(diagnostic)) => {
                                    last_diagnostic = diagnostic.clone().or(last_diagnostic);
                                    continue;
                                }
                                Err(_) => continue,
                            };
                            match assess(run, jobs, &check.declaration.required) {
                                Assessment::Wait => {}
                                Assessment::Missing(name) => {
                                    decided.insert(
                                        check.name.clone(),
                                        published.outcome(
                                            check,
                                            RemoteCheckReasonV1::RemoteCheckMissing,
                                            format!(
                                                "run {} attempt {} of `{}` completed without a \
                                                 job named {name:?}; correct the name in \
                                                 [checks.{}.remote] `required`",
                                                run.id, run.attempt, run.path, check.name
                                            ),
                                            None,
                                            Some(&pull_request),
                                        ),
                                    );
                                }
                                Assessment::Ambiguous(name) => {
                                    decided.insert(
                                        check.name.clone(),
                                        published.outcome(
                                            check,
                                            RemoteCheckReasonV1::RemoteCheckAmbiguous,
                                            format!(
                                                "job name {name:?} appears more than once in run \
                                                 {} attempt {} of `{}`; make job names unique",
                                                run.id, run.attempt, run.path
                                            ),
                                            None,
                                            Some(&pull_request),
                                        ),
                                    );
                                }
                                Assessment::Ready(jobs) => {
                                    ready.push((check, (*run).clone(), jobs));
                                }
                            }
                        }
                        several => {
                            decided.insert(
                                check.name.clone(),
                                published.outcome(
                                    check,
                                    RemoteCheckReasonV1::RemoteCheckAmbiguous,
                                    format!(
                                        "{} `pull_request` runs of `{}` exist for pull request \
                                         #{} at head commit {head_commit}; cancel the stray run",
                                        several.len(),
                                        check.declaration.workflow,
                                        pull.number
                                    ),
                                    None,
                                    Some(&pull_request),
                                ),
                            );
                        }
                    }
                }
            }
            Err(ToolError::Failed(diagnostic)) => {
                last_diagnostic = diagnostic.or(last_diagnostic);
            }
            Err(ToolError::Missing(_)) => {
                for check in undecided {
                    decided.insert(
                        check.name.clone(),
                        published.outcome(
                            check,
                            RemoteCheckReasonV1::RemoteToolUnavailable,
                            "`gh` disappeared from PATH during the wait; install the GitHub CLI"
                                .into(),
                            None,
                            Some(&pull_request),
                        ),
                    );
                }
                continue;
            }
            Err(_) => continue,
        }
        // The proof belongs to the observation it was read with: every batch of checks about
        // to be judged reads the pull request and its merge ref again, so a pull request
        // changed after an earlier check passed cannot carry a later one.
        let mut proven: Option<String> = None;
        if !ready.is_empty() {
            match prove(
                &api,
                &repository,
                &tools,
                url,
                &pull,
                &head_branch,
                &base_branch,
                &base_commit,
                &head_commit,
                &candidate_tree,
            ) {
                Ok(Proof::Proven(merge)) => {
                    stale = None;
                    proven = Some(merge);
                }
                Ok(Proof::Stale(why)) => stale = Some(why),
                Ok(Proof::Refused(why)) => {
                    let open: Vec<&RemoteCheckRequest> = phase
                        .checks
                        .iter()
                        .filter(|c| !decided.contains_key(&c.name))
                        .collect();
                    for check in open {
                        decided.insert(
                            check.name.clone(),
                            published.outcome(
                                check,
                                RemoteCheckReasonV1::RemoteMergeMismatch,
                                format!(
                                    "pull request #{} or its merge ref is not what af pushed \
                                     ({why}); someone changed the gate pull request: restore or \
                                     delete it",
                                    pull.number
                                ),
                                None,
                                Some(&pull_request),
                            ),
                        );
                    }
                    continue;
                }
                Err(ToolError::Failed(diagnostic)) => {
                    last_diagnostic = diagnostic.or(last_diagnostic);
                }
                Err(_) => {}
            }
        }
        if let Some(merge) = &proven {
            for (check, run, jobs) in ready {
                decided.insert(
                    check.name.clone(),
                    published.observed(check, &pull_request, merge, &run, jobs, &api),
                );
            }
        }
        if phase.checks.iter().all(|c| decided.contains_key(&c.name)) {
            break;
        }
        wait(&tools, settings.poll_interval);
    }
    Ok(phase
        .checks
        .iter()
        .filter_map(|check| decided.remove(&check.name))
        .collect())
}

/// Sleep up to `interval`, waking early for cancellation or the phase's end.
fn wait(tools: &Tools<'_>, interval: Duration) {
    let until = Instant::now() + interval.min(tools.remaining());
    while Instant::now() < until && !tools.cancelled() {
        std::thread::sleep(WAIT_SLICE.min(until.saturating_duration_since(Instant::now())));
    }
}

/// The first path under `.github/` whose presence, mode or content differs between the two
/// manifests, rendered for a message.
fn ci_difference(source: &Manifest, candidate: &Manifest) -> Option<String> {
    let under = |manifest: &Manifest| {
        manifest
            .entries
            .iter()
            .filter(|entry| review_core::decode_path(&entry.path).starts_with(b".github/"))
            .map(|entry| {
                (
                    entry.path.clone(),
                    (entry.kind.mode(), entry.content.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let (before, after) = (under(source), under(candidate));
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    paths
        .into_iter()
        .find(|path| before.get(*path) != after.get(*path))
        .map(|path| {
            let shown: String = String::from_utf8_lossy(&review_core::decode_path(path))
                .chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .take(200)
                .collect();
            shown
        })
}

/// The fields every published or observed outcome shares.
struct Published<'a> {
    base: &'a EvidenceBase<'a>,
    base_commit: &'a str,
    head_commit: &'a str,
    tree: &'a str,
}

impl Published<'_> {
    fn outcome(
        &self,
        check: &RemoteCheckRequest,
        reason: RemoteCheckReasonV1,
        message: String,
        diagnostic: Option<String>,
        pull_request: Option<&RemotePullRequestV1>,
    ) -> RemoteCheckOutcome {
        let mut evidence = self.base.evidence(
            &check.declaration,
            RemoteCheckStateV1::Published,
            Some(reason),
        );
        evidence.diagnostic = diagnostic;
        evidence.base_commit = Some(self.base_commit.into());
        evidence.head_commit = Some(self.head_commit.into());
        evidence.tree = Some(self.tree.into());
        evidence.pull_request = pull_request.cloned();
        RemoteCheckOutcome {
            name: check.name.clone(),
            evidence,
            message: Some(format!("remote check `{}`: {message}", check.name)),
            log: None,
        }
    }

    fn pull_failure(
        &self,
        check: &RemoteCheckRequest,
        pull_request: Option<&RemotePullRequestV1>,
        error: &ToolError,
    ) -> RemoteCheckOutcome {
        let (reason, message, diagnostic) = match error {
            ToolError::Cancelled => (
                RemoteCheckReasonV1::Cancelled,
                "the Attempt was cancelled while the gate pull request was opened".to_string(),
                None,
            ),
            ToolError::TimedOut => (
                RemoteCheckReasonV1::DeadlineExpired,
                "the remote phase ran out of time while the gate pull request was opened (raise \
                 check_process_wall_ms)"
                    .to_string(),
                None,
            ),
            ToolError::Missing(_) => (
                RemoteCheckReasonV1::RemoteToolUnavailable,
                "`gh` is not on PATH; install the GitHub CLI".to_string(),
                None,
            ),
            ToolError::Failed(diagnostic) => (
                RemoteCheckReasonV1::RemotePrRefused,
                "the draft gate pull request could not be found or opened; the diagnostic names \
                 why"
                .to_string(),
                diagnostic.clone(),
            ),
        };
        self.outcome(check, reason, message, diagnostic, pull_request)
    }

    fn observed(
        &self,
        check: &RemoteCheckRequest,
        pull_request: &RemotePullRequestV1,
        merge: &str,
        run: &Run,
        jobs: Vec<Job>,
        api: &Api<'_>,
    ) -> RemoteCheckOutcome {
        let github = api.github;
        let jobs: Vec<RemoteJobV1> = jobs
            .into_iter()
            .map(|job| {
                let url = Some(job.url.clone())
                    .filter(|url| url.starts_with("https://") && url.len() <= 512)
                    .unwrap_or_else(|| {
                        format!(
                            "https://github.com/{github}/actions/runs/{}/job/{}",
                            run.id, job.id
                        )
                    });
                let success = job.conclusion == "success";
                RemoteJobV1 {
                    id: job.id,
                    name: job.name,
                    conclusion: job.conclusion,
                    started_at: job.started_at,
                    completed_at: job.completed_at,
                    url,
                    steps: if success {
                        Vec::new()
                    } else {
                        job.unsuccessful_steps
                    },
                }
            })
            .collect();
        let failed: Vec<&RemoteJobV1> = jobs.iter().filter(|j| j.conclusion == "failure").collect();
        let other: Vec<&RemoteJobV1> = jobs
            .iter()
            .filter(|j| j.conclusion != "failure" && j.conclusion != "success")
            .collect();
        let (reason, message) = if !failed.is_empty() {
            (
                None,
                Some(format!(
                    "remote check `{}`: required job(s) {} concluded failure in run {} attempt {}; \
                     read the log at {}",
                    check.name,
                    failed
                        .iter()
                        .map(|job| format!("{:?}", job.name))
                        .collect::<Vec<_>>()
                        .join(", "),
                    run.id,
                    run.attempt,
                    failed[0].url
                )),
            )
        } else if let Some(job) = other.first() {
            (
                Some(RemoteCheckReasonV1::RemoteCheckInconclusive),
                Some(format!(
                    "remote check `{}`: required job {:?} concluded `{}` in run {} attempt {}; \
                     rerun it on GitHub, then resume the Task",
                    check.name, job.name, job.conclusion, run.id, run.attempt
                )),
            )
        } else {
            (None, None)
        };
        let mut evidence =
            self.base
                .evidence(&check.declaration, RemoteCheckStateV1::Observed, reason);
        evidence.base_commit = Some(self.base_commit.into());
        evidence.head_commit = Some(self.head_commit.into());
        evidence.tree = Some(self.tree.into());
        evidence.pull_request = Some(pull_request.clone());
        evidence.merge_commit = Some(merge.into());
        evidence.run = Some(RemoteRunV1 {
            id: run.id,
            attempt: run.attempt,
            workflow: run.path.clone(),
        });
        let log = log_excerpt(api, &jobs);
        evidence.jobs = jobs;
        RemoteCheckOutcome {
            name: check.name.clone(),
            evidence,
            message,
            log,
        }
    }
}

/// The log tails of the jobs that did not succeed, each under a header naming its job: at most
/// [`MAX_JOB_LOG_BYTES`] per job and [`MAX_CHECK_LOG_BYTES`] in all. A log that cannot be
/// fetched is left out and changes no outcome; `None` when nothing was kept.
fn log_excerpt(api: &Api<'_>, jobs: &[RemoteJobV1]) -> Option<Vec<u8>> {
    let mut excerpt = String::new();
    for job in jobs.iter().filter(|job| job.conclusion != "success") {
        let Ok(raw) = api.job_log(job.id) else {
            continue;
        };
        let header = format!("==> job {:?} ({}) {}\n", job.name, job.conclusion, job.url);
        let room = MAX_CHECK_LOG_BYTES.saturating_sub(excerpt.len() + header.len() + 1);
        let tail = log_tail(
            &api.tools.redactor.apply(&String::from_utf8_lossy(&raw)),
            room,
        );
        if tail.is_empty() {
            continue;
        }
        excerpt.push_str(&header);
        excerpt.push_str(tail.trim_end_matches('\n'));
        excerpt.push('\n');
    }
    (!excerpt.is_empty()).then(|| excerpt.into_bytes())
}

/// The last `MAX_JOB_LOG_BYTES` of `text`, and no more than `room`, starting at a line when the
/// text was cut.
fn log_tail(text: &str, room: usize) -> String {
    let keep = MAX_JOB_LOG_BYTES.min(room);
    if text.len() <= keep {
        return text.to_owned();
    }
    let mut start = text.len() - keep;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let tail = &text[start..];
    match tail.find('\n') {
        Some(newline) if newline + 1 < tail.len() => tail[newline + 1..].to_owned(),
        _ => tail.to_owned(),
    }
}

/// A pull request as read from the API.
#[derive(Debug, Clone)]
struct Pull {
    number: u64,
    url: String,
    open: bool,
    head_ref: String,
    head_sha: String,
    head_repo: String,
    base_ref: String,
    base_sha: String,
    base_repo: String,
}

#[derive(Debug, Clone)]
struct Run {
    id: u64,
    attempt: u64,
    path: String,
    event: String,
    head_sha: String,
    completed: bool,
    pull_requests: BTreeSet<u64>,
}

#[derive(Debug, Clone)]
struct Job {
    id: u64,
    name: String,
    completed: bool,
    conclusion: String,
    started_at: Option<String>,
    completed_at: Option<String>,
    url: String,
    unsuccessful_steps: Vec<RemoteStepV1>,
}

/// What the remote holds for this Task's gate branch pair.
enum FoundPull {
    Open(Pull),
    Closed(Pull),
    None,
}

enum Assessment {
    Wait,
    Missing(String),
    Ambiguous(String),
    Ready(Vec<Job>),
}

/// Judge one run's latest-attempt jobs against a check's required names.
fn assess(run: &Run, jobs: &[Job], required: &[String]) -> Assessment {
    for name in required {
        if jobs.iter().filter(|job| job.name == *name).count() > 1 {
            return Assessment::Ambiguous(name.clone());
        }
    }
    let mut ready = Vec::new();
    let mut waiting = false;
    for name in required {
        match jobs.iter().find(|job| job.name == *name) {
            None if run.completed => return Assessment::Missing(name.clone()),
            None => waiting = true,
            Some(job) if !job.completed => waiting = true,
            Some(job) => ready.push(job.clone()),
        }
    }
    if waiting {
        Assessment::Wait
    } else {
        Assessment::Ready(ready)
    }
}

enum Proof {
    /// The merge commit GitHub tested.
    Proven(String),
    /// Not yet what was pushed; re-read until the phase ends.
    Stale(String),
    /// Definitely not what was pushed.
    Refused(String),
}

/// Read the pull request and its merge ref back and require that GitHub tested exactly the
/// candidate tree merged onto this Task's base.
#[allow(clippy::too_many_arguments)]
fn prove(
    api: &Api<'_>,
    repository: &GateRepository,
    tools: &Tools<'_>,
    url: &str,
    pull: &Pull,
    head_branch: &str,
    base_branch: &str,
    base_commit: &str,
    head_commit: &str,
    candidate_tree: &str,
) -> Result<Proof, ToolError> {
    let current = api.pull(pull.number)?;
    if !current.open {
        return Ok(Proof::Refused("it is not open".into()));
    }
    if current.head_ref != head_branch
        || current.base_ref != base_branch
        || !current.head_repo.eq_ignore_ascii_case(api.github)
        || !current.base_repo.eq_ignore_ascii_case(api.github)
    {
        return Ok(Proof::Refused(
            "it joins other branches or repositories than the gate branches".into(),
        ));
    }
    if current.head_sha != head_commit || current.base_sha != base_commit {
        return Ok(Proof::Stale(
            "its head or base commit is not the pushed one".into(),
        ));
    }
    let Some(merge) = repository.fetch(tools, url, &format!("refs/pull/{}/merge", pull.number))?
    else {
        return Ok(Proof::Stale("its merge ref does not exist yet".into()));
    };
    let commit = repository.read_commit(tools, &merge)?;
    if commit.parents != [base_commit.to_owned(), head_commit.to_owned()] {
        return Ok(Proof::Stale(
            "its merge ref's parents are not the gate base and head commits".into(),
        ));
    }
    if commit.tree != candidate_tree {
        return Ok(Proof::Refused(
            "its merge ref's tree is not the candidate tree".into(),
        ));
    }
    Ok(Proof::Proven(merge))
}

/// The GitHub REST calls the executor makes, all through `gh api`.
struct Api<'a> {
    tools: &'a Tools<'a>,
    github: &'a str,
    cwd: &'a Path,
}

impl Api<'_> {
    fn call(&self, args: &[&str]) -> Result<Value, ToolError> {
        let mut full = vec!["api"];
        full.extend(args);
        let out = self.tools.run("gh", &full, None, false, self.cwd, &[])?;
        serde_json::from_slice(&out).map_err(|_| {
            ToolError::Failed(Some("`gh api` printed something other than JSON".into()))
        })
    }

    /// This Task's gate pull request, in whatever state: the open one when there is one,
    /// otherwise the newest closed one.
    fn find_pull(&self, head: &str, base: &str) -> Result<FoundPull, ToolError> {
        let owner = self.github.split('/').next().unwrap_or_default();
        let path = format!(
            "repos/{}/pulls?state=all&head={}&base={}&per_page=100",
            self.github,
            encode(&format!("{owner}:{head}")),
            encode(base)
        );
        let value = self.call(&[&path])?;
        let pulls = value.as_array().ok_or_else(|| {
            ToolError::Failed(Some("the pull request list is not an array".into()))
        })?;
        let mut found: Vec<Pull> = pulls
            .iter()
            .filter_map(|pull| parse_pull(pull, self.github))
            .filter(|pull| pull.head_ref == head && pull.base_ref == base)
            .collect();
        found.sort_by_key(|pull| pull.number);
        if let Some(open) = found.iter().find(|pull| pull.open) {
            return Ok(FoundPull::Open(open.clone()));
        }
        Ok(found.pop().map_or(FoundPull::None, FoundPull::Closed))
    }

    /// One job's log as GitHub serves it. It is plain text, not JSON.
    fn job_log(&self, job: u64) -> Result<Vec<u8>, ToolError> {
        let path = format!("repos/{}/actions/jobs/{job}/logs", self.github);
        self.tools
            .run("gh", &["api", &path], None, false, self.cwd, &[])
    }

    fn create_pull(&self, task_id: &str, head: &str, base: &str) -> Result<Pull, ToolError> {
        let path = format!("repos/{}/pulls", self.github);
        let title = format!("title={PR_TITLE_PREFIX}{task_id}");
        let head = format!("head={head}");
        let base = format!("base={base}");
        let body = format!("body={PR_BODY}");
        let value = self.call(&[
            "--method",
            "POST",
            &path,
            "-f",
            &title,
            "-f",
            &head,
            "-f",
            &base,
            "-f",
            &body,
            "-F",
            "draft=true",
        ])?;
        parse_pull(&value, self.github)
            .ok_or_else(|| ToolError::Failed(Some("the created pull request is malformed".into())))
    }

    fn pull(&self, number: u64) -> Result<Pull, ToolError> {
        let value = self.call(&[&format!("repos/{}/pulls/{number}", self.github)])?;
        parse_pull(&value, self.github)
            .ok_or_else(|| ToolError::Failed(Some("the pull request is malformed".into())))
    }

    fn runs(&self, head_commit: &str) -> Result<Vec<Run>, ToolError> {
        let mut runs = Vec::new();
        let mut seen = 0_u64;
        for page in 1..=MAX_PAGES {
            let value = self.call(&[&format!(
                "repos/{}/actions/runs?event=pull_request&head_sha={head_commit}&per_page=100&page={page}",
                self.github
            )])?;
            let listed = value["workflow_runs"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let total = value["total_count"].as_u64().unwrap_or(0);
            seen += listed.len() as u64;
            let empty = listed.is_empty();
            runs.extend(listed.iter().filter_map(parse_run));
            if empty || seen >= total {
                return Ok(runs);
            }
        }
        // A listing the page limit did not exhaust proves no run unique: nothing is judged.
        Err(unexhausted("workflow runs"))
    }

    fn jobs(&self, run: u64, attempt: u64) -> Result<Vec<Job>, ToolError> {
        let mut jobs = Vec::new();
        let mut seen = 0_u64;
        for page in 1..=MAX_PAGES {
            let value = self.call(&[&format!(
                "repos/{}/actions/runs/{run}/attempts/{attempt}/jobs?per_page=100&page={page}",
                self.github
            )])?;
            let listed = value["jobs"].as_array().cloned().unwrap_or_default();
            let total = value["total_count"].as_u64().unwrap_or(0);
            seen += listed.len() as u64;
            let empty = listed.is_empty();
            jobs.extend(listed.iter().filter_map(parse_job));
            if empty || seen >= total {
                return Ok(jobs);
            }
        }
        // As for runs: a job name is unique only in a listing read to its end.
        Err(unexhausted("jobs"))
    }
}

/// The failure of a listing that still had entries after [`MAX_PAGES`] pages of 100.
fn unexhausted(what: &str) -> ToolError {
    ToolError::Failed(Some(format!(
        "the remote lists more than {} {what}; af reads no further and judges nothing from a \
         partial listing",
        MAX_PAGES * 100
    )))
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn parse_pull(value: &Value, github: &str) -> Option<Pull> {
    let number = value["number"].as_u64().filter(|n| *n > 0)?;
    let url = Some(text(&value["html_url"]))
        .filter(|url| url.starts_with("https://") && url.len() <= 512 && !url.contains(' '))
        .unwrap_or_else(|| format!("https://github.com/{github}/pull/{number}"));
    Some(Pull {
        number,
        url,
        open: value["state"] == "open",
        head_ref: text(&value["head"]["ref"]),
        head_sha: text(&value["head"]["sha"]),
        head_repo: text(&value["head"]["repo"]["full_name"]),
        base_ref: text(&value["base"]["ref"]),
        base_sha: text(&value["base"]["sha"]),
        base_repo: text(&value["base"]["repo"]["full_name"]),
    })
}

fn parse_run(value: &Value) -> Option<Run> {
    Some(Run {
        id: value["id"].as_u64().filter(|id| *id > 0)?,
        attempt: value["run_attempt"].as_u64().unwrap_or(1).max(1),
        path: text(&value["path"]),
        event: text(&value["event"]),
        head_sha: text(&value["head_sha"]),
        completed: value["status"] == "completed",
        pull_requests: value["pull_requests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|pull| pull["number"].as_u64())
            .collect(),
    })
}

/// A name as evidence keeps it: control characters replaced, bounded to 128 characters.
fn bounded_name(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_REMOTE_NAME_CHARS)
        .collect();
    if name.trim().is_empty() {
        "(unnamed)".into()
    } else {
        name
    }
}

fn conclusion(value: &Value) -> String {
    let text = text(value);
    if (1..=32).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        text
    } else {
        "unknown".into()
    }
}

fn timestamp(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|t| {
            t.len() >= 20
                && t.len() <= 30
                && t.ends_with('Z')
                && t.as_bytes()[10] == b'T'
                && t.bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b':' | b'T' | b'Z' | b'.'))
        })
        .map(str::to_owned)
}

fn parse_job(value: &Value) -> Option<Job> {
    let completed = value["status"] == "completed";
    let unsuccessful_steps = value["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|step| step["conclusion"].is_string() && step["conclusion"] != "success")
        .take(MAX_REMOTE_STEPS)
        .map(|step| RemoteStepV1 {
            name: bounded_name(step["name"].as_str().unwrap_or_default()),
            conclusion: conclusion(&step["conclusion"]),
        })
        .collect();
    Some(Job {
        id: value["id"].as_u64().filter(|id| *id > 0)?,
        name: value["name"].as_str()?.to_owned(),
        completed,
        conclusion: conclusion(&value["conclusion"]),
        started_at: timestamp(&value["started_at"]),
        completed_at: timestamp(&value["completed_at"]),
        url: text(&value["html_url"]),
        unsuccessful_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(completed: bool) -> Run {
        Run {
            id: 1,
            attempt: 1,
            path: ".github/workflows/ci.yml".into(),
            event: "pull_request".into(),
            head_sha: "a".repeat(40),
            completed,
            pull_requests: BTreeSet::from([1]),
        }
    }

    fn job(name: &str, completed: bool, conclusion: &str) -> Job {
        parse_job(&json!({"id": 3, "name": name, "status": if completed {"completed"} else {"in_progress"},
            "conclusion": conclusion, "html_url": "https://github.com/o/r/actions/runs/1/job/3",
            "steps": [{"name": "Checkout", "conclusion": "success"},
                      {"name": "Test\u{7}", "conclusion": "failure"}]}))
        .unwrap()
    }

    #[test]
    fn jobs_are_judged_only_when_complete_unique_and_present() {
        let required = vec!["lint".to_string(), "test".to_string()];
        let both = [job("lint", true, "success"), job("test", true, "failure")];
        assert!(
            matches!(assess(&run(true), &both, &required), Assessment::Ready(jobs) if jobs.len() == 2)
        );
        let pending = [job("lint", true, "success"), job("test", false, "")];
        assert!(matches!(
            assess(&run(false), &pending, &required),
            Assessment::Wait
        ));
        let absent = [job("lint", true, "success")];
        assert!(matches!(
            assess(&run(false), &absent, &required),
            Assessment::Wait
        ));
        assert!(
            matches!(assess(&run(true), &absent, &required), Assessment::Missing(name) if name == "test")
        );
        let twice = [
            job("lint", true, "success"),
            job("lint", true, "success"),
            job("test", true, "success"),
        ];
        assert!(
            matches!(assess(&run(true), &twice, &required), Assessment::Ambiguous(name) if name == "lint")
        );
    }

    #[test]
    fn only_unsuccessful_steps_are_kept_by_name() {
        let parsed = job("test", true, "failure");
        assert_eq!(
            parsed.unsuccessful_steps,
            vec![RemoteStepV1 {
                name: "Test ".into(),
                conclusion: "failure".into()
            }]
        );
        assert_eq!(conclusion(&json!("Weird Value")), "unknown");
        assert_eq!(conclusion(&json!(null)), "unknown");
    }

    #[test]
    fn a_ci_difference_names_the_first_path() {
        use review_source_git::{Entry, EntryKind};
        let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
        let entry = |path: &str, kind, content| Entry {
            path: path.into(),
            kind,
            content,
            size: 1,
        };
        let source = Manifest::new(vec![
            entry(".github/workflows/ci.yml", EntryKind::File, digest('1')),
            entry("src/a.rs", EntryKind::File, digest('2')),
        ])
        .unwrap();
        let same_ci = Manifest::new(vec![
            entry(".github/workflows/ci.yml", EntryKind::File, digest('1')),
            entry("src/a.rs", EntryKind::File, digest('3')),
        ])
        .unwrap();
        assert_eq!(ci_difference(&source, &same_ci), None);
        let mode = Manifest::new(vec![entry(
            ".github/workflows/ci.yml",
            EntryKind::Executable,
            digest('1'),
        )])
        .unwrap();
        assert_eq!(
            ci_difference(&source, &mode).as_deref(),
            Some(".github/workflows/ci.yml")
        );
        let added = Manifest::new(vec![
            entry(".github/workflows/ci.yml", EntryKind::File, digest('1')),
            entry(".github/x.yml", EntryKind::File, digest('1')),
        ])
        .unwrap();
        assert_eq!(
            ci_difference(&source, &added).as_deref(),
            Some(".github/x.yml")
        );
    }
}
