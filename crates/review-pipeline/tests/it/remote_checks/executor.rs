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
    assert!(!setup.remote.calls().contains("logs"));
}

#[test]
fn a_failure_keeps_unsuccessful_step_names_and_never_a_log() {
    let setup = setup();
    setup.remote.serve_runs("runs-pull-request.json");
    setup.remote.serve_jobs(77, 1, "jobs-failure.json");
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
    let recorded = serde_json::to_string(&outcome.evidence).unwrap();
    assert!(!recorded.contains("SECRET-LOG-LINE"));
    assert!(!setup.remote.calls().contains("logs"));
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
    for variant in ["another base", "merge commit", "foreign message"] {
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
            _ => as_af(
                bare,
                &["commit-tree", &tree, "-p", &base, "-m", "fix the build"],
            ),
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
        ("other-parents", Duration::from_secs(10)),
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
