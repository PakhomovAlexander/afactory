//! The `github-pr` executor driven directly through its seam: every refusal reason, the
//! transport's ownership rules and the wait's judgement.

use super::*;
use review_core::task::remote_check::RemoteCheckVerdictV1;

const CI: &str = ".github/workflows/ci.yml";
const LIMIT: Duration = Duration::from_secs(30);

struct Setup {
    _cas_directory: tempfile::TempDir,
    cas: Cas,
    remote: Remote,
    snapshots: Snapshots,
}

fn setup() -> Setup {
    let cas_directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(cas_directory.path().join("cas")).unwrap();
    let snapshots = Snapshots::new(&cas);
    Setup {
        _cas_directory: cas_directory,
        cas,
        remote: Remote::new(),
        snapshots,
    }
}

impl Setup {
    fn run(&self, candidate: &(String, Manifest), owner: &str) -> RemoteCheckOutcome {
        let mut outcomes = phase(
            &self.cas,
            &self.remote,
            &self.snapshots,
            candidate,
            owner,
            TASK,
            &[kernel_request(CI)],
            LIMIT,
            None,
        )
        .unwrap();
        assert_eq!(outcomes.len(), 1);
        outcomes.remove(0)
    }

    fn passing(&self) {
        self.remote.serve_runs("runs-pull-request.json");
        self.remote.serve_jobs(77, 1, "jobs-success.json");
    }
}

/// The tree of `commit` in the bare repository equals `manifest` entry for entry: path, mode
/// and bytes.
fn assert_tree_is(remote: &Remote, cas: &Cas, commit: &str, manifest: &Manifest) {
    let listed = git(
        &remote.bare,
        &["ls-tree", "-r", "-z", "--full-tree", commit],
    );
    let mut actual: Vec<(Vec<u8>, String, Vec<u8>)> = listed
        .split('\0')
        .filter(|record| !record.is_empty())
        .map(|record| {
            let (header, path) = record.split_once('\t').unwrap();
            let fields: Vec<&str> = header.split(' ').collect();
            let blob = std::process::Command::new("git")
                .args([
                    "--git-dir",
                    remote.bare.to_str().unwrap(),
                    "cat-file",
                    "blob",
                    fields[2],
                ])
                .output()
                .unwrap()
                .stdout;
            (path.as_bytes().to_vec(), fields[0].to_string(), blob)
        })
        .collect();
    let mut expected: Vec<(Vec<u8>, String, Vec<u8>)> = manifest
        .entries
        .iter()
        .map(|entry| {
            (
                review_core::decode_path(&entry.path),
                entry.kind.mode().to_string(),
                cas.get(&entry.content).unwrap(),
            )
        })
        .collect();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn a_passing_run_is_observed_on_the_exact_candidate_tree() {
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    let evidence = &outcome.evidence;
    assert_eq!(evidence.verdict(), RemoteCheckVerdictV1::Passed);
    assert_eq!(evidence.snapshot_id, candidate.0);
    assert_eq!(evidence.source_snapshot_id, setup.snapshots.source_id);
    assert_eq!(evidence.pull_request.as_ref().unwrap().number, 12);
    let run = evidence.run.as_ref().unwrap();
    assert_eq!((run.id, run.attempt, run.workflow.as_str()), (77, 1, CI));
    assert_eq!(
        evidence
            .jobs
            .iter()
            .map(|job| job.name.as_str())
            .collect::<Vec<_>>(),
        [LINT, CHECK]
    );
    let head = setup.remote.branch("head").unwrap();
    let base = setup.remote.branch("base").unwrap();
    assert_eq!(evidence.head_commit.as_deref(), Some(head.as_str()));
    assert_eq!(evidence.base_commit.as_deref(), Some(base.as_str()));
    let merge = git(&setup.remote.bare, &["rev-parse", "refs/pull/12/merge"]);
    assert_eq!(evidence.merge_commit.as_deref(), Some(merge.as_str()));
    // The pushed head commit's tree and the merge commit's tree are the candidate manifest.
    assert_tree_is(&setup.remote, &setup.cas, &head, &candidate.1);
    assert_tree_is(&setup.remote, &setup.cas, &merge, &candidate.1);
    assert_tree_is(&setup.remote, &setup.cas, &base, &setup.snapshots.source);
    assert_eq!(
        git(
            &setup.remote.bare,
            &["rev-parse", &format!("{head}^{{tree}}")]
        ),
        evidence.tree.clone().unwrap()
    );
    // Gate commits carry a fixed identity and name their Task, owner and Snapshot.
    let base_commit = git(&setup.remote.bare, &["cat-file", "commit", &base]);
    assert!(!base_commit.contains("\nparent "), "{base_commit}");
    assert!(base_commit.contains("author af <af@localhost> 0 +0000"));
    assert!(base_commit.contains(&format!("Af-Task-Owner: {OWNER}")));
    assert!(base_commit.contains(&format!("Af-Snapshot: {}", setup.snapshots.source_id)));
    let head_commit = git(&setup.remote.bare, &["cat-file", "commit", &head]);
    assert!(head_commit.contains(&format!("parent {base}")));
    assert!(head_commit.contains(&format!("Af-Snapshot: {}", candidate.0)));
    // Exactly two refs were pushed, both under this Task's prefix; the merge ref is GitHub's.
    let refs = git(&setup.remote.bare, &["for-each-ref", "--format=%(refname)"]);
    assert_eq!(refs.lines().count(), 3, "{refs}");
    for line in refs.lines() {
        let name = line.split(' ').next().unwrap();
        assert!(
            name.starts_with(&format!("refs/heads/af-gate/{TASK}/"))
                || name == "refs/pull/12/merge",
            "{name}"
        );
    }
    assert_eq!(setup.remote.pulls_created(), 1);
    assert!(setup.remote.calls().contains("draft=true"));
    assert_eq!(outcome.log, None, "a passing check keeps no log");
    assert!(!setup.remote.calls().contains("/logs"));
}

#[test]
fn a_failure_keeps_unsuccessful_step_names_and_the_failed_jobs_log_tail() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-failure.json");
    setup.remote.serve_log(
        1001,
        b"the lint job succeeded; its log is never asked for\n",
    );
    let log = format!(
        "2026-10-04T10:00:01Z cloning {}\n2026-10-04T10:12:30Z FAIL [ 12.3s] af::it task_repair\n\
         2026-10-04T10:12:31Z error: test run failed\n",
        setup.remote.push_url()
    );
    setup.remote.serve_log(1002, log.as_bytes());
    let candidate = setup.snapshots.candidate(&setup.cas, "broken\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    assert_eq!(outcome.evidence.verdict(), RemoteCheckVerdictV1::Failed);
    let failed = &outcome.evidence.jobs[1];
    assert_eq!(failed.conclusion, "failure");
    assert_eq!(
        failed
            .steps
            .iter()
            .map(|step| (step.name.as_str(), step.conclusion.as_str()))
            .collect::<Vec<_>>(),
        [("Run make check", "failure"), ("Upload report", "skipped")]
    );
    assert!(outcome.evidence.jobs[0].steps.is_empty());
    let message = outcome.message.clone().unwrap();
    assert!(
        message.contains(CHECK) && message.contains(&failed.url),
        "{message}"
    );
    // The failed job's log tail is kept under a header naming the job, with the push URL
    // redacted like every other kept text; a successful job's log is never fetched.
    let kept = String::from_utf8(outcome.log.clone().expect("a log excerpt")).unwrap();
    assert!(
        kept.starts_with(&format!("==> job {CHECK:?} (failure) {}\n", failed.url)),
        "{kept}"
    );
    assert!(kept.contains("FAIL [ 12.3s] af::it task_repair") && kept.ends_with("failed\n"));
    assert!(kept.contains("cloning <push-url>") && !kept.contains(&setup.remote.push_url()));
    let calls = setup.remote.calls();
    assert!(calls.contains("actions/jobs/1002/logs") && !calls.contains("actions/jobs/1001/logs"));
    let recorded = serde_json::to_string(&outcome.evidence).unwrap();
    assert!(
        !recorded.contains("task_repair"),
        "evidence holds no log text"
    );
}

#[test]
fn a_long_log_keeps_its_bounded_tail_and_a_missing_log_changes_nothing() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-failure.json");
    let candidate = setup.snapshots.candidate(&setup.cas, "broken\n");
    // No log served: the job still failed, and nothing is kept.
    let without = setup.run(&candidate, OWNER);
    assert_eq!(without.evidence.verdict(), RemoteCheckVerdictV1::Failed);
    assert_eq!(without.log, None);
    // A log far past the bound: only its tail is kept, cut at a line.
    let mut long = String::new();
    for line in 0..40_000 {
        long.push_str(&format!("line {line:05} of the failing job\n"));
    }
    assert!(long.len() > 4 * github_pr::MAX_JOB_LOG_BYTES);
    setup.remote.serve_log(1002, long.as_bytes());
    let with = setup.run(&candidate, OWNER);
    assert_eq!(with.evidence.verdict(), RemoteCheckVerdictV1::Failed);
    let kept = String::from_utf8(with.log.expect("a log excerpt")).unwrap();
    let (header, tail) = kept.split_once('\n').unwrap();
    assert!(header.starts_with("==> job "), "{header}");
    assert!(tail.len() <= github_pr::MAX_JOB_LOG_BYTES, "{}", tail.len());
    assert!(tail.len() > github_pr::MAX_JOB_LOG_BYTES - 64);
    assert!(tail.starts_with("line ") && tail.ends_with("line 39999 of the failing job\n"));
    assert!(kept.len() <= github_pr::MAX_CHECK_LOG_BYTES);
}

#[test]
fn a_job_that_neither_succeeded_nor_failed_is_inconclusive() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-cancelled.json");
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Observed,
        Some(RemoteCheckReasonV1::RemoteCheckInconclusive),
    );
    let message = outcome.message.unwrap();
    assert!(
        message.contains("`cancelled`") && message.contains("rerun"),
        "{message}"
    );
}

#[test]
fn a_candidate_that_changes_the_workflow_is_never_sent() {
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.derived(
        &setup.cas,
        ".github/workflows/ci.yml",
        EntryKind::File,
        b"on: push\n",
    );
    let outcome = setup.run(&candidate, OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteCandidateChangesCi),
    );
    let message = outcome.message.unwrap();
    assert!(message.contains(".github/workflows/ci.yml") && message.contains("checks"));
    assert_eq!(setup.remote.refs(), "");
    assert_eq!(setup.remote.calls(), "", "gh was never called");
    // A new file under .github/ is a difference too.
    let added = setup.snapshots.derived(
        &setup.cas,
        ".github/CODEOWNERS",
        EntryKind::File,
        b"* @octo\n",
    );
    expect(
        &setup.run(&added, OWNER),
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteCandidateChangesCi),
    );
}

#[test]
fn a_task_id_that_is_not_a_ref_component_is_refused() {
    let setup = setup();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcomes = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &candidate,
        OWNER,
        "bad..id",
        &[kernel_request(CI)],
        LIMIT,
        None,
    )
    .unwrap();
    expect(
        &outcomes[0],
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteRefInvalid),
    );
    assert!(
        outcomes[0]
            .message
            .as_ref()
            .unwrap()
            .contains("ref-safe Task ID")
    );
    assert_eq!(setup.remote.refs(), "");
}

#[test]
fn missing_or_unauthenticated_tools_are_refused_with_their_fix() {
    let setup = setup();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let run_with = |path: Option<std::ffi::OsString>| {
        let target = setup.remote.target();
        let mut settings = setup.remote.settings();
        if path.is_some() {
            settings.path = path;
        }
        let mut outcomes = github_pr::run(
            &RemotePhase {
                cas: &setup.cas,
                task_id: TASK,
                owner: OWNER,
                candidate_id: &candidate.0,
                candidate: &candidate.1,
                source_id: &setup.snapshots.source_id,
                source: &setup.snapshots.source,
                target: &target,
                mapping: None,
                checks: &[kernel_request(CI)],
                deadline: Instant::now() + LIMIT,
                cancellation: None,
            },
            &settings,
        )
        .unwrap();
        outcomes.remove(0)
    };
    // No `git` at all.
    let empty = tempfile::tempdir().unwrap();
    let outcome = run_with(Some(empty.path().as_os_str().to_owned()));
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteToolUnavailable),
    );
    assert!(outcome.message.unwrap().contains("install Git"));
    // `git` but no `gh`: the PATH holds only a `git` that delegates to the real one.
    let only_git = tempfile::tempdir().unwrap();
    let wrapper = only_git.path().join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nPATH='{}'; export PATH\nexec git \"$@\"\n",
            std::env::var("PATH").unwrap()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let outcome = run_with(Some(only_git.path().as_os_str().to_owned()));
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteToolUnavailable),
    );
    assert!(outcome.message.unwrap().contains("install the GitHub CLI"));
    // `gh` that is not logged in.
    setup.remote.flag("unauthenticated");
    let outcome = run_with(None);
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteToolUnavailable),
    );
    assert!(outcome.message.unwrap().contains("gh auth login"));
    assert_eq!(
        outcome.evidence.diagnostic, None,
        "gh auth output is never kept"
    );
    assert_eq!(setup.remote.refs(), "");
}

#[test]
fn a_rejected_push_keeps_its_diagnostic_with_the_url_redacted() {
    let setup = setup();
    let hook = setup.remote.bare.join("hooks/pre-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho \"ruleset: pushes to {} need review\" >&2\nexit 1\n",
            setup.remote.push_url()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemotePushRefused),
    );
    let diagnostic = outcome.evidence.diagnostic.clone().unwrap();
    assert!(diagnostic.contains("<push-url>"), "{diagnostic}");
    assert!(diagnostic.contains("ruleset"), "{diagnostic}");
    let recorded = serde_json::to_string(&outcome.evidence).unwrap()
        + outcome.message.as_deref().unwrap_or_default();
    assert!(!recorded.contains(&setup.remote.push_url()), "{recorded}");
    assert!(!recorded.contains(setup.remote.directory.path().to_str().unwrap()));
    assert_eq!(setup.remote.refs(), "", "the atomic push wrote nothing");
}

#[test]
fn the_same_attempt_twice_attaches_without_a_second_pull_request_or_commit() {
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let first = setup.run(&candidate, OWNER);
    expect(&first, RemoteCheckStateV1::Observed, None);
    let refs = setup.remote.refs();
    let second = setup.run(&candidate, OWNER);
    expect(&second, RemoteCheckStateV1::Observed, None);
    assert_eq!(setup.remote.refs(), refs, "nothing was pushed");
    assert_eq!(setup.remote.pulls_created(), 1);
    assert_eq!(first.evidence.head_commit, second.evidence.head_commit);
    let head = setup.remote.branch("head").unwrap();
    assert_eq!(
        git(&setup.remote.bare, &["rev-list", "--count", &head]),
        "2"
    );
}

#[test]
fn a_repair_round_appends_one_commit_to_the_same_pull_request() {
    let setup = setup();
    setup.passing();
    let first = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(&first, RemoteCheckStateV1::Observed, None);
    let repaired = setup.snapshots.candidate(&setup.cas, "version 3\n");
    let second = setup.run(&repaired, OWNER);
    expect(&second, RemoteCheckStateV1::Observed, None);
    let head = setup.remote.branch("head").unwrap();
    assert_eq!(second.evidence.head_commit.as_deref(), Some(head.as_str()));
    assert_eq!(
        git(&setup.remote.bare, &["rev-parse", &format!("{head}^")]),
        first.evidence.head_commit.clone().unwrap()
    );
    assert_eq!(
        git(&setup.remote.bare, &["rev-list", "--count", &head]),
        "3"
    );
    assert_eq!(first.evidence.base_commit, second.evidence.base_commit);
    assert_eq!(setup.remote.pulls_created(), 1);
    assert_tree_is(&setup.remote, &setup.cas, &head, &repaired.1);
}

/// `git` with the fixed identity every gate commit carries.
fn as_af(directory: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "af")
        .env("GIT_AUTHOR_EMAIL", "af@localhost")
        .env("GIT_AUTHOR_DATE", "@0 +0000")
        .env("GIT_COMMITTER_NAME", "af")
        .env("GIT_COMMITTER_EMAIL", "af@localhost")
        .env("GIT_COMMITTER_DATE", "@0 +0000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn a_head_branch_this_task_did_not_make_is_refused_and_nothing_is_pushed() {
    for variant in [
        "another base",
        "merge commit",
        "foreign message",
        "foreign snapshot",
        "another tree than its snapshot",
    ] {
        let setup = setup();
        setup.passing();
        let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
        expect(
            &setup.run(&candidate, OWNER),
            RemoteCheckStateV1::Observed,
            None,
        );
        let base = setup.remote.branch("base").unwrap();
        let head = setup.remote.branch("head").unwrap();
        let bare = &setup.remote.bare;
        let tree = git(bare, &["rev-parse", &format!("{head}^{{tree}}")]);
        let message = git(bare, &["log", "-1", "--format=%B", &head]);
        // Forged with af's own fixed identity, so only the named difference remains.
        let forged = match variant {
            // The candidate tree and this Task's head message, on another root commit.
            "another base" => {
                let other = as_af(bare, &["commit-tree", &tree, "-m", "another base"]);
                as_af(bare, &["commit-tree", &tree, "-p", &other, "-m", &message])
            }
            "merge commit" => as_af(
                bare,
                &[
                    "commit-tree",
                    &tree,
                    "-p",
                    &base,
                    "-p",
                    &head,
                    "-m",
                    &message,
                ],
            ),
            "foreign message" => as_af(
                bare,
                &["commit-tree", &tree, "-p", &base, "-m", "fix the build"],
            ),
            // Gate-shaped in every respect — identity, Task, owner, even the candidate tree —
            // but naming a Snapshot this Task's Store never held.
            "foreign snapshot" => {
                let foreign = message.replace(
                    &format!("Af-Snapshot: {}", candidate.0),
                    &format!("Af-Snapshot: sha256:{}", "7".repeat(64)),
                );
                assert_ne!(foreign, message);
                as_af(bare, &["commit-tree", &tree, "-p", &head, "-m", &foreign])
            }
            // This Task's own head message, on a tree that is not that Snapshot's.
            _ => {
                let other = git(bare, &["rev-parse", &format!("{base}^{{tree}}")]);
                as_af(bare, &["commit-tree", &other, "-p", &head, "-m", &message])
            }
        };
        git(
            bare,
            &[
                "update-ref",
                &format!("refs/heads/af-gate/{TASK}/head"),
                &forged,
            ],
        );
        let refs = setup.remote.refs();
        let outcome = setup.run(&candidate, OWNER);
        expect(
            &outcome,
            RemoteCheckStateV1::Refused,
            Some(RemoteCheckReasonV1::RemoteRefConflict),
        );
        assert!(
            outcome.message.unwrap().contains("delete both branches"),
            "{variant}"
        );
        assert_eq!(setup.remote.refs(), refs, "{variant}: nothing was pushed");
    }
}

#[test]
fn the_same_task_id_from_another_store_is_refused_at_the_base() {
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    expect(
        &setup.run(&candidate, OWNER),
        RemoteCheckStateV1::Observed,
        None,
    );
    let refs = setup.remote.refs();
    let outcome = setup.run(&candidate, OTHER_STORE);
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemoteRefConflict),
    );
    assert!(outcome.message.unwrap().contains("another Store"));
    assert_eq!(setup.remote.refs(), refs);
    assert_eq!(setup.remote.pulls_created(), 1);
}

#[test]
fn runs_of_another_event_or_workflow_earn_nothing() {
    let setup = setup();
    setup.remote.serve_runs("runs-push-and-other-workflow.json");
    setup.remote.serve_jobs(78, 1, "jobs-success.json");
    setup.remote.serve_jobs(79, 1, "jobs-success.json");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteCheckMissing),
    );
    assert!(outcome.message.unwrap().contains("trigger"));
}

#[test]
fn a_run_with_two_attempts_is_judged_on_the_latest() {
    let setup = setup();
    setup.remote.serve_runs("runs-two-attempts.json");
    setup.remote.serve_jobs(77, 1, "jobs-failure.json");
    setup.remote.serve_jobs(77, 2, "jobs-success.json");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    assert_eq!(outcome.evidence.verdict(), RemoteCheckVerdictV1::Passed);
    assert_eq!(outcome.evidence.run.as_ref().unwrap().attempt, 2);
    assert!(!setup.remote.calls().contains("/attempts/1/"));
}

#[test]
fn duplicated_job_names_and_two_runs_of_one_workflow_are_ambiguous() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-duplicate.json");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteCheckAmbiguous),
    );
    assert!(outcome.message.unwrap().contains(LINT));

    let setup = self::setup();
    setup.remote.serve_runs("runs-two-of-one-workflow.json");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteCheckAmbiguous),
    );
    assert!(outcome.message.unwrap().contains("stray run"));
}

#[test]
fn a_completed_run_without_a_required_job_is_missing() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-in-progress.json");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteCheckMissing),
    );
    assert!(outcome.message.unwrap().contains("`required`"));
}

#[test]
fn a_merge_ref_with_another_tree_or_other_parents_is_refused() {
    for (mode, limit) in [
        ("other-tree", LIMIT),
        ("other-parents", Duration::from_secs(20)),
    ] {
        let setup = setup();
        setup.passing();
        setup.remote.merge_mode(mode);
        let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
        let started = Instant::now();
        let outcomes = phase(
            &setup.cas,
            &setup.remote,
            &setup.snapshots,
            &candidate,
            OWNER,
            TASK,
            &[kernel_request(CI)],
            limit,
            None,
        )
        .unwrap();
        expect(
            &outcomes[0],
            RemoteCheckStateV1::Published,
            Some(RemoteCheckReasonV1::RemoteMergeMismatch),
        );
        if mode == "other-tree" {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "refused at once"
            );
        }
        assert!(
            outcomes[0]
                .message
                .as_ref()
                .unwrap()
                .contains("restore or delete")
        );
    }
}

#[test]
fn a_pull_request_that_cannot_be_opened_is_refused_with_a_redacted_diagnostic() {
    let setup = setup();
    setup.remote.flag("refuse-pull");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemotePrRefused),
    );
    let diagnostic = outcome.evidence.diagnostic.clone().unwrap();
    assert!(diagnostic.contains("<push-url>"), "{diagnostic}");
    assert!(!diagnostic.contains(&setup.remote.push_url()));
}

#[test]
fn two_remote_checks_share_one_clock() {
    let setup = setup();
    setup.remote.serve_runs("runs-two-workflows.json");
    setup.remote.serve_jobs(77, 1, "jobs-success.json");
    setup.remote.serve_jobs(80, 1, "jobs-in-progress.json");
    let slow = RemoteCheckRequest {
        name: "slow".into(),
        declaration: RemoteCheckV1 {
            executor: RemoteExecutorV1::GithubPr,
            workflow: ".github/workflows/slow.yml".into(),
            required: vec!["slow / build".into()],
        },
    };
    let limit = Duration::from_secs(10);
    let started = Instant::now();
    let outcomes = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &setup.snapshots.candidate(&setup.cas, "version 2\n"),
        OWNER,
        TASK,
        &[kernel_request(CI), slow],
        limit,
        None,
    )
    .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(outcomes[0].name, "kernel");
    expect(&outcomes[0], RemoteCheckStateV1::Observed, None);
    assert_eq!(outcomes[0].evidence.verdict(), RemoteCheckVerdictV1::Passed);
    assert_eq!(outcomes[1].name, "slow");
    expect(
        &outcomes[1],
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::DeadlineExpired),
    );
    assert!(
        elapsed >= limit && elapsed < limit + Duration::from_secs(5),
        "{elapsed:?}"
    );
    assert_eq!(
        setup.remote.pulls_created(),
        1,
        "one pull request for both checks"
    );
}

#[test]
fn a_closed_gate_pull_request_is_never_replaced_by_a_second_one() {
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    expect(
        &setup.run(&candidate, OWNER),
        RemoteCheckStateV1::Observed,
        None,
    );
    setup.remote.pull_state("closed");
    let refs = setup.remote.refs();
    let outcome = setup.run(&candidate, OWNER);
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemotePrRefused),
    );
    assert_eq!(setup.remote.pulls_created(), 1, "no second pull request");
    assert_eq!(setup.remote.refs(), refs);
    assert_eq!(outcome.evidence.pull_request.as_ref().unwrap().number, 12);
    let message = outcome.message.unwrap();
    assert!(message.contains("gh pr reopen 12"), "{message}");
}

#[test]
fn a_merge_proof_is_read_again_for_every_batch_of_checks() {
    let setup = setup();
    setup.remote.serve_runs("runs-two-workflows.json");
    setup.remote.serve_jobs(77, 1, "jobs-success.json");
    setup.remote.serve_jobs(80, 1, "jobs-in-progress.json");
    let slow = RemoteCheckRequest {
        name: "slow".into(),
        declaration: RemoteCheckV1 {
            executor: RemoteExecutorV1::GithubPr,
            workflow: ".github/workflows/slow.yml".into(),
            required: vec!["slow / build".into()],
        },
    };
    // After `kernel` has passed on a good merge ref, the pull request's merge ref changes and
    // only then does the slow job succeed: the earlier proof must not carry the later check.
    let state = setup.remote.state.clone();
    let changer = std::thread::spawn(move || {
        // Wait for the first proof and then for the next poll to begin: by then `kernel` has
        // been judged on the good merge ref, however loaded the machine is.
        let waited = Instant::now();
        loop {
            let calls = std::fs::read_to_string(state.join("calls.log")).unwrap_or_default();
            let proven = calls.find("repos/octo/gate/pulls/12");
            if proven.is_some_and(|at| calls[at..].contains("actions/runs?")) {
                break;
            }
            assert!(waited.elapsed() < LIMIT, "the first proof never happened");
            std::thread::sleep(Duration::from_millis(20));
        }
        Remote::replace_in(&state, "merge-mode", b"other-tree");
        let done = json!({"total_count": 1, "jobs": [{
            "id": 1004, "run_id": 80, "run_attempt": 1, "name": "slow / build",
            "status": "completed", "conclusion": "success",
            "started_at": "2026-10-04T10:00:00Z", "completed_at": "2026-10-04T10:05:00Z",
            "html_url": "https://github.com/octo/gate/actions/runs/80/job/1004",
            "steps": [{"name": "Set up job", "status": "completed",
                       "conclusion": "success", "number": 1}]}]});
        Remote::replace_in(&state, "jobs-80-1.json", done.to_string().as_bytes());
    });
    let outcomes = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &setup.snapshots.candidate(&setup.cas, "version 2\n"),
        OWNER,
        TASK,
        &[kernel_request(CI), slow],
        Duration::from_secs(60),
        None,
    )
    .unwrap();
    changer.join().unwrap();
    expect(&outcomes[0], RemoteCheckStateV1::Observed, None);
    assert_eq!(outcomes[0].evidence.verdict(), RemoteCheckVerdictV1::Passed);
    expect(
        &outcomes[1],
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteMergeMismatch),
    );
}

#[test]
fn an_interrupted_push_that_landed_is_recorded_as_published_and_resume_attaches() {
    let setup = setup();
    setup.passing();
    // The remote accepts the atomic update and only then stalls: the client is stopped by the
    // phase deadline after both branches exist.
    let hooks = setup.remote.bare.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("post-receive");
    std::fs::write(&hook, "#!/bin/sh\nsleep 120\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &setup.snapshots.candidate(&setup.cas, "version 2\n"),
        OWNER,
        TASK,
        &[kernel_request(CI)],
        Duration::from_secs(12),
        None,
    );
    assert!(
        setup.remote.branch("base").is_some() && setup.remote.branch("head").is_some(),
        "the push landed before the client was stopped"
    );
    // The recovery read-back saw both branches as pushed, so the record says `published`,
    // never `refused`; nothing was judged after the deadline.
    let outcomes = result.unwrap();
    expect(
        &outcomes[0],
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::DeadlineExpired),
    );
    assert_eq!(
        outcomes[0].evidence.head_commit,
        setup.remote.branch("head")
    );
    assert_eq!(outcomes[0].evidence.pull_request, None);
    let message = outcomes[0].message.clone().unwrap();
    assert!(
        message.contains("the push landed") && message.contains("resume"),
        "{message}"
    );
    assert_eq!(
        setup.remote.pulls_created(),
        0,
        "nothing ran after the deadline"
    );
    std::fs::remove_file(&hook).unwrap();
    let resumed = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"), OWNER);
    expect(&resumed, RemoteCheckStateV1::Observed, None);
    assert_eq!(setup.remote.pulls_created(), 1);
}

#[test]
fn a_listing_the_page_limit_does_not_exhaust_judges_nothing() {
    let setup = setup();
    setup.passing();
    // The remote claims far more runs than ten pages hold: a second run of the workflow could
    // sit beyond them, so the one that was read is not known to be the only one.
    let runs = std::fs::read_to_string(
        workspace_root().join("fixtures/remote-checks/github/runs-pull-request.json"),
    )
    .unwrap()
    .replace("\"total_count\": 1", "\"total_count\": 1001");
    assert!(runs.contains("1001"));
    setup.remote.replace("runs.json", runs.as_bytes());
    let outcomes = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &setup.snapshots.candidate(&setup.cas, "version 2\n"),
        OWNER,
        TASK,
        &[kernel_request(CI)],
        Duration::from_secs(8),
        None,
    )
    .unwrap();
    expect(
        &outcomes[0],
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::DeadlineExpired),
    );
    let diagnostic = outcomes[0].evidence.diagnostic.clone().unwrap();
    assert!(
        diagnostic.contains("more than 1000 workflow runs"),
        "{diagnostic}"
    );
}

#[test]
fn cancellation_during_the_wait_ends_the_subprocesses() {
    let setup = setup();
    setup.remote.flag("runs-hang");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = cancel.clone();
    let state = setup.remote.state.clone();
    let canceller = std::thread::spawn(move || {
        let waited = Instant::now();
        while !state.join("hang.pid").exists() && waited.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::Release);
    });
    let started = Instant::now();
    let outcomes = phase(
        &setup.cas,
        &setup.remote,
        &setup.snapshots,
        &setup.snapshots.candidate(&setup.cas, "version 2\n"),
        OWNER,
        TASK,
        &[kernel_request(CI)],
        Duration::from_secs(60),
        Some(&cancel),
    )
    .unwrap();
    canceller.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the hung call was ended"
    );
    expect(
        &outcomes[0],
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::Cancelled),
    );
    let pid = std::fs::read_to_string(setup.remote.state.join("hang.pid")).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the hung `gh` process {pid} still runs");
}

/// What a cleanup is handed: the repository, commits and pull request one outcome recorded.
fn recorded(outcome: &RemoteCheckOutcome) -> github_pr::GateEvidence {
    github_pr::GateEvidence {
        github: outcome.evidence.github.clone(),
        base_commit: outcome.evidence.base_commit.clone(),
        head_commit: outcome.evidence.head_commit.clone(),
        pull_requests: outcome
            .evidence
            .pull_request
            .iter()
            .map(|pull| pull.number)
            .collect(),
    }
}

/// ADR-0144: a cleanup proves ownership first. A head branch another pusher moved is left, with
/// the pull request whose head moved with it, while the base branch that still holds the
/// recorded commit goes; the reason names what was left.
#[test]
fn a_replaced_head_branch_and_its_pull_request_are_left_and_named() {
    use review_core::task::remote_check::GateCleanupOutcomeV1;
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    let base = setup.remote.branch("base").unwrap();
    git(
        &setup.remote.bare,
        &[
            "update-ref",
            &format!("refs/heads/af-gate/{TASK}/head"),
            &base,
        ],
    );
    let cleanup = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    cleanup.validate(TASK).unwrap();
    assert_eq!(cleanup.outcome, GateCleanupOutcomeV1::Failed, "{cleanup:?}");
    let reason = cleanup.reason.clone().unwrap();
    assert!(
        reason.contains(&format!(
            "branch af-gate/{TASK}/head was left: it holds {base}"
        )),
        "{reason}"
    );
    assert!(
        reason.contains("pull request #12 was left open"),
        "{reason}"
    );
    assert_eq!(setup.remote.branch("head").as_deref(), Some(base.as_str()));
    assert!(
        setup.remote.branch("base").is_none(),
        "the recorded base goes"
    );
    let calls = setup.remote.calls();
    assert!(!calls.contains("PATCH"), "{calls}");
    assert!(
        !calls.contains("--force") && !calls.contains(" +"),
        "{calls}"
    );
}

/// A pull request whose head commit is not the recorded one is left open; a target that names
/// another repository than the evidence touches nothing at all.
#[test]
fn a_pull_request_that_moved_or_a_foreign_target_is_left() {
    use review_core::task::remote_check::GateCleanupOutcomeV1;
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    let mut foreign = recorded(&outcome);
    foreign.github = "octo/other".into();
    let untouched = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &foreign,
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    assert_eq!(untouched.outcome, GateCleanupOutcomeV1::Failed);
    assert!(
        untouched
            .reason
            .as_deref()
            .unwrap()
            .contains("not the recorded github:octo/other"),
        "{untouched:?}"
    );
    assert!(setup.remote.branch("head").is_some() && setup.remote.branch("base").is_some());
    std::fs::write(setup.remote.state.join("pull-head-sha"), "f".repeat(40)).unwrap();
    let cleanup = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    assert_eq!(cleanup.outcome, GateCleanupOutcomeV1::Failed, "{cleanup:?}");
    let reason = cleanup.reason.clone().unwrap();
    assert!(
        reason.contains(&format!(
            "pull request #12 was left open: its head commit {} is not the recorded",
            "f".repeat(40)
        )),
        "{reason}"
    );
    assert!(!setup.remote.calls().contains("PATCH"));
}

/// ADR-0144: a finished Task's cleanup closes its gate pull request and deletes exactly its two
/// branches; a branch of another Task and the pull request's other state stay as they were.
#[test]
fn the_cleanup_closes_the_gate_pull_request_and_deletes_only_this_tasks_branches() {
    use review_core::task::remote_check::GateCleanupOutcomeV1;
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    let head = setup.remote.branch("head").unwrap();
    git(
        &setup.remote.bare,
        &["update-ref", "refs/heads/af-gate/another-task/head", &head],
    );
    let cleanup = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    cleanup.validate(TASK).unwrap();
    assert_eq!(cleanup.outcome, GateCleanupOutcomeV1::Done, "{cleanup:?}");
    assert_eq!(cleanup.pull_requests, [12]);
    assert!(setup.remote.branch("head").is_none());
    assert!(setup.remote.branch("base").is_none());
    assert_eq!(
        setup.remote.refs(),
        format!("refs/heads/af-gate/another-task/head {head}")
    );
    let calls = setup.remote.calls();
    assert!(
        calls.contains("--method PATCH repos/octo/gate/pulls/12 -f state=closed"),
        "{calls}"
    );
    // Run again on what is left: nothing to close or delete, and still done.
    let again = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    assert_eq!(again.outcome, GateCleanupOutcomeV1::Done, "{again:?}");
}

/// A cleanup that cannot close the pull request says why, with the push URL redacted, and a
/// later attempt finishes it.
#[test]
fn a_failed_cleanup_keeps_its_redacted_reason_and_a_retry_finishes_it() {
    use review_core::task::remote_check::GateCleanupOutcomeV1;
    let setup = setup();
    setup.passing();
    let candidate = setup.snapshots.candidate(&setup.cas, "version 2\n");
    let outcome = setup.run(&candidate, OWNER);
    expect(&outcome, RemoteCheckStateV1::Observed, None);
    setup.remote.flag("refuse-close");
    let failed = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    failed.validate(TASK).unwrap();
    assert_eq!(failed.outcome, GateCleanupOutcomeV1::Failed);
    let reason = failed.reason.clone().unwrap();
    assert!(reason.contains("closing pull request #12"), "{reason}");
    assert!(!reason.contains(&setup.remote.push_url()), "{reason}");
    let record = serde_json::to_string(&failed).unwrap();
    assert!(!record.contains(&setup.remote.push_url()), "{record}");
    std::fs::remove_file(setup.remote.state.join("refuse-close")).unwrap();
    let retried = github_pr::cleanup(
        TASK,
        &setup.remote.target(),
        &recorded(&outcome),
        None,
        &setup.remote.settings(),
        Instant::now() + LIMIT,
    );
    assert_eq!(retried.outcome, GateCleanupOutcomeV1::Done, "{retried:?}");
    assert!(setup.remote.branch("head").is_none());
}
