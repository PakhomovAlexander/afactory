//! The gate pull request's `no-perf` label (ADR-0196): set only for a candidate whose whole
//! change from its source lies in the operator's `no_perf_paths`, removed before a head update
//! that no longer qualifies, never created in the repository, and never changed on a pull
//! request that is not provably this Task's. Every other label stays.

use super::*;
use review_core::task::remote_check::RemoteCheckVerdictV1;

const CI: &str = ".github/workflows/ci.yml";
const LIMIT: Duration = Duration::from_secs(30);
const PROSE: &str = "docs/guide.md";

struct Setup {
    _cas_directory: tempfile::TempDir,
    cas: Cas,
    remote: Remote,
    snapshots: Snapshots,
}

/// A remote whose repository has the `no-perf` label and whose mapping entry lists
/// `docs/*.md`, with a passing run served for whatever head is pushed.
fn setup() -> Setup {
    let cas_directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(cas_directory.path().join("cas")).unwrap();
    let snapshots = Snapshots::new(&cas);
    let remote = Remote::new();
    remote.serve_runs("runs-pull-request.json");
    remote.serve_jobs(77, 1, "jobs-success.json");
    remote.labels_of("repo-labels", &["documentation", "no-perf"]);
    *remote.no_perf_paths.borrow_mut() = vec!["docs/*.md".into()];
    Setup {
        _cas_directory: cas_directory,
        cas,
        remote,
        snapshots,
    }
}

impl Setup {
    fn run_within(&self, candidate: &(String, Manifest), limit: Duration) -> RemoteCheckOutcome {
        let mut outcomes = phase(
            &self.cas,
            &self.remote,
            &self.snapshots,
            candidate,
            OWNER,
            TASK,
            &[kernel_request(CI)],
            limit,
            None,
        )
        .unwrap();
        assert_eq!(outcomes.len(), 1);
        outcomes.remove(0)
    }

    fn run(&self, candidate: &(String, Manifest)) -> RemoteCheckOutcome {
        self.run_within(candidate, LIMIT)
    }

    fn prose(&self, text: &str) -> (String, Manifest) {
        self.snapshots
            .derived(&self.cas, PROSE, EntryKind::File, text.as_bytes())
    }

    fn labels(&self) -> Vec<String> {
        self.remote.label_lines("labels")
    }

    fn passed(outcome: &RemoteCheckOutcome) {
        expect(outcome, RemoteCheckStateV1::Observed, None);
        assert_eq!(outcome.evidence.verdict(), RemoteCheckVerdictV1::Passed);
    }
}

#[test]
fn a_prose_only_candidate_labels_its_own_gate_pull_request_and_keeps_its_other_labels() {
    let setup = setup();
    setup.remote.labels_of("labels", &["documentation"]);
    let outcome = setup.run(&setup.prose("A guide.\n"));
    Setup::passed(&outcome);
    assert_eq!(setup.labels(), ["documentation", "no-perf"]);
    let head = setup.remote.branch("head").unwrap();
    assert_eq!(
        setup.remote.label_lines("labels-added"),
        [format!("no-perf {head}")]
    );
    assert!(setup.remote.label_lines("labels-created").is_empty());
    assert!(setup.remote.label_lines("labels-removed").is_empty());
    // The same candidate again changes nothing about the label.
    let again = setup.run(&setup.prose("A guide.\n"));
    Setup::passed(&again);
    assert_eq!(setup.remote.label_lines("labels-added").len(), 1);
    assert_eq!(setup.labels(), ["documentation", "no-perf"]);
}

#[test]
fn a_repair_that_no_longer_qualifies_removes_the_stale_label_before_its_head_is_pushed() {
    let setup = setup();
    setup.remote.labels_of("labels", &["documentation"]);
    let first = setup.run(&setup.prose("A guide.\n"));
    Setup::passed(&first);
    let labeled_head = first.evidence.head_commit.clone().unwrap();
    assert_eq!(setup.labels(), ["documentation", "no-perf"]);

    // The repair round also changes code: the whole change from the source decides.
    let repaired = setup.snapshots.derived_many(
        &setup.cas,
        &[
            (PROSE, EntryKind::File, b"A guide.\n"),
            ("src/lib.txt", EntryKind::File, b"version 2\n"),
        ],
    );
    let second = setup.run(&repaired);
    Setup::passed(&second);
    assert_ne!(second.evidence.head_commit.as_deref(), Some(&*labeled_head));
    // The label left while the pull request still showed the earlier head.
    assert_eq!(
        setup.remote.label_lines("labels-removed"),
        [format!("no-perf {labeled_head}")]
    );
    assert_eq!(setup.labels(), ["documentation"]);
    // A later phase of the same candidate never brings it back.
    Setup::passed(&setup.run(&repaired));
    assert_eq!(setup.labels(), ["documentation"]);
    assert_eq!(setup.remote.label_lines("labels-added").len(), 1);
}

#[test]
fn a_prose_only_last_round_does_not_qualify_a_cumulative_change_to_code() {
    let setup = setup();
    Setup::passed(&setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n")));
    // Only the documentation differs from the previous round, but the code differs from the
    // Task's source.
    let next = setup.snapshots.derived_many(
        &setup.cas,
        &[
            ("src/lib.txt", EntryKind::File, b"version 2\n"),
            (PROSE, EntryKind::File, b"A guide.\n"),
        ],
    );
    Setup::passed(&setup.run(&next));
    assert!(setup.labels().is_empty());
    assert!(setup.remote.label_lines("labels-added").is_empty());
}

#[test]
fn a_label_the_repository_does_not_have_is_never_created() {
    for repo_labels in [vec!["documentation"], vec![]] {
        let setup = setup();
        setup.remote.labels_of("repo-labels", &repo_labels);
        Setup::passed(&setup.run(&setup.prose("A guide.\n")));
        assert!(setup.labels().is_empty());
        assert!(setup.remote.label_lines("labels-created").is_empty());
        assert!(!setup.remote.calls().contains("issues/12/labels"));
    }
    // A label of another spelling is not `no-perf`, and is not applied for it either.
    let setup = setup();
    setup.remote.labels_of("repo-labels", &[]);
    std::fs::write(setup.remote.state.join("repo-label-spelled"), "No-Perf").unwrap();
    Setup::passed(&setup.run(&setup.prose("A guide.\n")));
    assert!(setup.labels().is_empty());
    assert!(!setup.remote.calls().contains("issues/12/labels"));
}

#[test]
fn a_label_that_cannot_be_read_or_added_changes_no_outcome_and_claims_nothing() {
    for flag in ["label-read-fails", "refuse-label-add"] {
        let setup = setup();
        setup.remote.flag(flag);
        let outcome = setup.run(&setup.prose("A guide.\n"));
        Setup::passed(&outcome);
        assert!(setup.labels().is_empty(), "{flag}");
        assert!(setup.remote.label_lines("labels-created").is_empty());
    }
}

#[test]
fn a_stale_label_that_cannot_be_removed_stops_the_head_update_and_fails_closed() {
    let setup = setup();
    Setup::passed(&setup.run(&setup.prose("A guide.\n")));
    let labeled_head = setup.remote.branch("head").unwrap();
    setup.remote.flag("refuse-label-remove");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"));
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemotePushRefused),
    );
    let message = outcome.message.clone().unwrap();
    assert!(
        message.contains("head update was not pushed") && message.contains("no-perf"),
        "{message}"
    );
    assert!(
        outcome
            .evidence
            .diagnostic
            .as_deref()
            .is_some_and(|d| d.contains("HTTP 403"))
    );
    assert_eq!(setup.remote.branch("head"), Some(labeled_head));
    assert_eq!(setup.labels(), ["no-perf"]);
}

#[test]
fn a_stale_label_on_a_new_gate_pull_request_is_removed_or_fails_every_check() {
    // Someone put the label on before af resolved the pull request; the candidate is code.
    let setup = setup();
    setup
        .remote
        .labels_of("labels", &["no-perf", "documentation"]);
    Setup::passed(&setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n")));
    assert_eq!(setup.labels(), ["documentation"]);

    let setup = self::setup();
    setup.remote.labels_of("labels", &["no-perf"]);
    setup.remote.flag("refuse-label-remove");
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"));
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemotePrRefused),
    );
    assert_eq!(outcome.evidence.pull_request.as_ref().unwrap().number, 12);
    let message = outcome.message.clone().unwrap();
    assert!(message.contains("could not be removed"), "{message}");
    assert_eq!(setup.labels(), ["no-perf"]);
}

#[test]
fn without_no_perf_paths_nothing_is_labeled_and_a_stale_label_is_removed() {
    let setup = setup();
    setup.remote.no_perf_paths.borrow_mut().clear();
    Setup::passed(&setup.run(&setup.prose("A guide.\n")));
    assert!(setup.labels().is_empty());
    assert!(!setup.remote.calls().contains("labels"));

    let setup = self::setup();
    setup.remote.no_perf_paths.borrow_mut().clear();
    setup
        .remote
        .labels_of("labels", &["documentation", "no-perf"]);
    Setup::passed(&setup.run(&setup.prose("A guide.\n")));
    assert_eq!(setup.labels(), ["documentation"]);
}

#[test]
fn the_label_of_a_pull_request_that_is_not_provably_this_tasks_is_never_changed() {
    // A head someone else moved the pull request to: neither added nor removed.
    let setup = setup();
    std::fs::write(setup.remote.state.join("pull-head-sha"), "f".repeat(40)).unwrap();
    let outcome = setup.run_within(&setup.prose("A guide.\n"), Duration::from_secs(2));
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteMergeMismatch),
    );
    assert!(setup.labels().is_empty());

    let setup = self::setup();
    setup.remote.labels_of("labels", &["no-perf"]);
    std::fs::write(setup.remote.state.join("pull-head-sha"), "f".repeat(40)).unwrap();
    let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"));
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemotePrRefused),
    );
    let message = outcome.message.clone().unwrap();
    assert!(message.contains("not provably this Task's"), "{message}");
    assert_eq!(setup.labels(), ["no-perf"]);
    assert!(setup.remote.label_lines("labels-removed").is_empty());
}

#[test]
fn a_rejected_neutral_update_never_exempts_the_old_runtime_head() {
    let setup = setup();
    Setup::passed(&setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n")));
    let old_head = setup.remote.branch("head");
    let hook = setup.remote.bare.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let outcome = setup.run(&setup.prose("A guide.\n"));
    expect(
        &outcome,
        RemoteCheckStateV1::Refused,
        Some(RemoteCheckReasonV1::RemotePushRefused),
    );
    assert_eq!(setup.remote.branch("head"), old_head);
    assert!(setup.labels().is_empty());
    assert!(setup.remote.label_lines("labels-added").is_empty());
    // Once the push succeeds, the exemption belongs to the new head only.
    std::fs::remove_file(hook).unwrap();
    Setup::passed(&setup.run(&setup.prose("A guide.\n")));
    assert_eq!(
        setup.remote.label_lines("labels-added"),
        [format!("no-perf {}", setup.remote.branch("head").unwrap())]
    );
}

#[test]
fn an_unreadable_pull_or_label_list_prevents_a_stale_exemption_reaching_a_new_head() {
    for flag in [
        "pull-list-fails",
        "labels-malformed",
        "pull-record-malformed",
    ] {
        let setup = setup();
        Setup::passed(&setup.run(&setup.prose("A guide.\n")));
        let old_head = setup.remote.branch("head");
        setup.remote.flag(flag);
        let outcome = setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n"));
        expect(
            &outcome,
            RemoteCheckStateV1::Refused,
            Some(RemoteCheckReasonV1::RemotePushRefused),
        );
        assert_eq!(setup.remote.branch("head"), old_head, "{flag}");
        assert_eq!(setup.labels(), ["no-perf"], "{flag}");
    }
}

#[test]
fn a_successful_push_does_not_exempt_a_pull_request_still_showing_the_old_runtime_head() {
    let setup = setup();
    Setup::passed(&setup.run(&setup.snapshots.candidate(&setup.cas, "version 2\n")));
    let old_head = setup.remote.branch("head").unwrap();
    let stale = setup.remote.state.join("pull-head-sha");
    std::fs::write(&stale, &old_head).unwrap();
    let candidate = setup.prose("A guide.\n");
    let outcome = setup.run_within(&candidate, Duration::from_secs(5));
    expect(
        &outcome,
        RemoteCheckStateV1::Published,
        Some(RemoteCheckReasonV1::RemoteMergeMismatch),
    );
    let new_head = setup.remote.branch("head").unwrap();
    assert_ne!(new_head, old_head, "the push succeeded");
    assert_eq!(
        outcome.evidence.head_commit.as_deref(),
        Some(new_head.as_str())
    );
    assert!(setup.labels().is_empty());
    assert!(setup.remote.label_lines("labels-added").is_empty());
    // A retry may exempt the candidate once the PR API catches up to the pushed head.
    std::fs::remove_file(stale).unwrap();
    Setup::passed(&setup.run(&candidate));
    assert_eq!(
        setup.remote.label_lines("labels-added"),
        [format!("no-perf {new_head}")]
    );
}
