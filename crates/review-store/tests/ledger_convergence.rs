//! Ledger convergence scenarios: how re-reports, resolutions and Rounds move one Finding.
//!
//! Each test folds the events a Review host appends — `RoundStarted@1` for Scope authority,
//! `GenerationAdvanced@1` for the Round counter, `FindingReported@1` over an immutable
//! `FindingReport@1` artifact, `FindingResolved@1` — through `Ledger::apply_event`, and pins
//! the projected Finding and the convergence verdict under the default policy (one clean Round,
//! at most three, gate `major`).

use review_core::{
    EventType, FindingReport, Location, RoundStartedPayloadV1, RunEvent, Severity, SubjectV1,
};
use review_store::ledger::TransitionKind;
use review_store::{Cas, Convergence, ConvergencePolicy, Finding, Ledger, Status, Verdict};

const KEY: &str = "claim";

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn event(
    event_type: EventType,
    payload: serde_json::Value,
    artifact_refs: Vec<String>,
) -> RunEvent {
    RunEvent {
        event_id: String::new(),
        run_id: "run".into(),
        sequence: 0,
        event_type,
        occurred_at: "2026-09-22T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs,
        payload,
    }
}

struct Run {
    _dir: tempfile::TempDir,
    cas: Cas,
    ledger: Ledger,
    subject_id: String,
}

impl Run {
    /// A whole-tree Campaign whose first Round has started.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let subject_id = cas
            .put_json(&serde_json::to_value(SubjectV1::whole_tree(digest('c'))).unwrap())
            .unwrap();
        let mut run = Self {
            _dir: dir,
            cas,
            ledger: Ledger::default(),
            subject_id,
        };
        run.start_round(1);
        run
    }

    fn start_round(&mut self, round: u32) {
        let payload = RoundStartedPayloadV1 {
            round,
            epoch: 1,
            campaign_manifest_id: digest('d'),
            subject_id: self.subject_id.clone(),
            prior_finding_set_id: digest('e'),
            prior_demand_set_id: digest('f'),
        };
        self.apply(event(
            EventType::RoundStartedV1,
            serde_json::to_value(payload).unwrap(),
            Vec::new(),
        ));
        self.apply(event(
            EventType::GenerationAdvancedV1,
            serde_json::json!({ "round": round }),
            Vec::new(),
        ));
    }

    fn next_round(&mut self) {
        self.start_round(self.ledger.round + 1);
    }

    /// One reviewer's Report of the claim in the current Round.
    fn report(&mut self, source: &str, severity: Severity, body: &str) -> String {
        let report = FindingReport {
            title: "Retry loop can spin forever".into(),
            severity,
            locations: vec![Location::file("src/retry.rs")],
            body: body.into(),
            fix: "bound the retries".into(),
            confidence: 0.9,
            failure_trace: None,
            rule_id: None,
            occurrence_key: None,
            relations: Vec::new(),
        };
        let report_id = self
            .cas
            .put_json(&serde_json::to_value(report).unwrap())
            .unwrap();
        self.apply(event(
            EventType::FindingReportedV1,
            serde_json::json!({
                "key": KEY,
                "round": self.ledger.round,
                "source": source,
                "report_id": report_id,
            }),
            vec![report_id.clone()],
        ));
        report_id
    }

    fn resolve(&mut self, status: Status, note: &str) {
        self.apply(event(
            EventType::FindingResolvedV1,
            serde_json::json!({
                "key": KEY,
                "status": status.as_str(),
                "note": note,
                "round": self.ledger.round,
            }),
            Vec::new(),
        ));
    }

    fn apply(&mut self, event: RunEvent) {
        self.ledger.apply_event(&event, &self.cas).unwrap();
    }

    fn finding(&self) -> &Finding {
        self.ledger.get(KEY).expect("the claim is projected")
    }

    fn kinds(&self) -> Vec<TransitionKind> {
        self.finding().history.iter().map(|t| t.kind).collect()
    }

    fn convergence(&self) -> Convergence {
        self.ledger.convergence(ConvergencePolicy::default())
    }
}

/// (round, open_blocking, new_recent, verdict)
fn verdict(convergence: Convergence) -> (u32, usize, usize, Verdict) {
    (
        convergence.round,
        convergence.open_blocking,
        convergence.new_recent,
        convergence.verdict,
    )
}

/// A Finding fixed in one Round and re-reported in a later one reopens with the new evidence,
/// and the note the fix recorded survives beside the reopen note.
#[test]
fn a_fix_that_did_not_hold_reopens_and_keeps_the_fix_note() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.resolve(
        Status::Fixed,
        "fixed by clamping the range in commit abc1234",
    );
    run.next_round();
    run.report(
        "deep-r2",
        Severity::Major,
        "r2: the fix only moved the boundary",
    );

    let finding = run.finding();
    assert_eq!(finding.status, Status::Open);
    assert_eq!((finding.news_round, finding.last_seen_round), (2, 2));
    assert_eq!(finding.source, "deep-r2");
    assert_eq!(finding.body, "r2: the fix only moved the boundary");
    assert_eq!(finding.reports.len(), 2);
    assert_eq!(
        run.kinds(),
        [
            TransitionKind::Reported,
            TransitionKind::Resolved(Status::Fixed),
            TransitionKind::Reopened,
        ]
    );
    let notes: Vec<&str> = finding
        .history
        .iter()
        .filter_map(|t| t.note.as_deref())
        .collect();
    assert_eq!(
        notes,
        [
            "fixed by clamping the range in commit abc1234",
            "reopened: re-reported by deep-r2 in round 2",
        ],
        "the reopen must not overwrite what the fix claimed"
    );
    assert_eq!(
        finding.current_note(),
        Some("reopened: re-reported by deep-r2 in round 2")
    );
    assert_eq!(verdict(run.convergence()), (2, 1, 1, Verdict::NotConverged));
}

/// A higher-severity re-report of an open Finding adopts the rank, evidence and source in place
/// and counts as news, so it forces another clean Round.
#[test]
fn a_higher_severity_re_report_escalates_in_place_and_is_news() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1: looks like noise");
    run.next_round();
    run.report(
        "cross-r2",
        Severity::Blocker,
        "r2: the log ships to a third party",
    );

    let finding = run.finding();
    assert_eq!(finding.status, Status::Open);
    assert_eq!(finding.severity, Severity::Blocker);
    assert_eq!((finding.news_round, finding.last_seen_round), (2, 2));
    assert_eq!(finding.source, "cross-r2");
    assert_eq!(finding.body, "r2: the log ships to a third party");
    assert_eq!(
        run.kinds(),
        [TransitionKind::Reported, TransitionKind::Escalated]
    );
    assert_eq!(
        finding.current_note(),
        Some("escalated: re-reported as blocker by cross-r2 in round 2")
    );
    assert_eq!(verdict(run.convergence()), (2, 1, 1, Verdict::NotConverged));
}

/// A same-severity re-report in a later Round only moves `last_seen_round`: it is not news and
/// does not restart the clean window, and the first reporter's evidence stays adopted.
#[test]
fn a_same_severity_re_report_in_a_later_round_is_not_news() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.next_round();
    run.report("cross-r2", Severity::Major, "r2 evidence, same rank");

    let finding = run.finding();
    assert_eq!(finding.status, Status::Open);
    assert_eq!((finding.news_round, finding.last_seen_round), (1, 2));
    assert_eq!(finding.source, "deep-r1");
    assert_eq!(finding.body, "r1 evidence");
    assert_eq!(finding.reports.len(), 2);
    assert_eq!(
        run.kinds(),
        [TransitionKind::Reported, TransitionKind::Duplicate]
    );
    assert_eq!(verdict(run.convergence()), (2, 1, 0, Verdict::NotConverged));
}

/// Two reviewers reporting the claim in one Round leave one Finding credited to the first
/// reporter, with both Reports attached as distinct immutable artifacts.
#[test]
fn a_same_round_duplicate_keeps_both_reports() {
    let mut run = Run::new();
    let deep = run.report("deep-r1", Severity::Major, "deep: no backoff, no cap");
    let cross = run.report(
        "cross-r1",
        Severity::Major,
        "cross: independently found, different evidence",
    );

    assert_eq!(run.ledger.len(), 1, "still one finding");
    let finding = run.finding();
    assert_eq!(finding.source, "deep-r1");
    assert_eq!(finding.body, "deep: no backoff, no cap");
    let reports: Vec<(&str, &str)> = finding
        .reports
        .iter()
        .map(|r| (r.source.as_str(), r.report_id.as_str()))
        .collect();
    assert_eq!(
        reports,
        [("deep-r1", deep.as_str()), ("cross-r1", cross.as_str())]
    );
    assert_ne!(deep, cross, "different evidence must be distinct artifacts");
    assert_eq!(
        run.kinds(),
        [TransitionKind::Reported, TransitionKind::Duplicate]
    );
}

/// A rejected Finding is never reopened by a same-severity re-report — reviewers rediscover
/// declined claims forever — and the rejection reason stays the current note.
#[test]
fn a_rejected_claim_re_reported_at_the_same_severity_stays_rejected() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.resolve(
        Status::Rejected,
        "not a defect: the executor is single-threaded here",
    );
    run.next_round();
    run.report("cross-r2", Severity::Major, "r2: same claim, same rank");

    let finding = run.finding();
    assert_eq!(finding.status, Status::Rejected);
    assert_eq!(finding.severity, Severity::Major);
    assert_eq!((finding.news_round, finding.last_seen_round), (1, 2));
    assert_eq!(finding.source, "deep-r1");
    assert_eq!(finding.body, "r1 evidence");
    assert_eq!(
        finding.current_note(),
        Some("not a defect: the executor is single-threaded here")
    );
    assert_eq!(
        run.kinds(),
        [
            TransitionKind::Reported,
            TransitionKind::Resolved(Status::Rejected),
            TransitionKind::Duplicate,
        ]
    );
    assert_eq!(verdict(run.convergence()), (2, 0, 0, Verdict::Converged));
}

/// A rejected Finding re-reported at a higher severity adopts the rank, evidence and source in
/// place so a later re-triage sees the real severity, but its status is untouched. It is news.
#[test]
fn a_rejected_claim_re_reported_higher_adopts_the_rank_but_stays_rejected() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1: cosmetic");
    run.resolve(Status::Rejected, "rejected at major: clamped downstream");
    run.next_round();
    run.report(
        "cross-r2",
        Severity::Blocker,
        "r2: it disables the deadline entirely",
    );

    let finding = run.finding();
    assert_eq!(finding.status, Status::Rejected);
    assert_eq!(finding.severity, Severity::Blocker);
    assert_eq!((finding.news_round, finding.last_seen_round), (2, 2));
    assert_eq!(finding.source, "cross-r2");
    assert_eq!(finding.body, "r2: it disables the deadline entirely");
    assert_eq!(
        finding.current_note(),
        Some("rejected at major: clamped downstream")
    );
    assert_eq!(
        run.kinds(),
        [
            TransitionKind::Reported,
            TransitionKind::Resolved(Status::Rejected),
            TransitionKind::AdoptedWhileDeclined,
        ]
    );
    assert_eq!(verdict(run.convergence()), (2, 0, 1, Verdict::NotConverged));
}

/// `wontfix` follows the same never-reopen rule as `rejected`.
#[test]
fn a_wontfix_claim_is_never_reopened_by_a_re_report() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.resolve(Status::Wontfix, "codec is deleted in the next release");
    run.next_round();
    run.report("deep-r2", Severity::Major, "r2: rediscovered");

    let finding = run.finding();
    assert_eq!(finding.status, Status::Wontfix);
    assert_eq!((finding.news_round, finding.last_seen_round), (1, 2));
    assert_eq!(finding.source, "deep-r1");
    assert_eq!(finding.body, "r1 evidence");
    assert_eq!(
        finding.current_note(),
        Some("codec is deleted in the next release")
    );
    assert_eq!(
        run.kinds(),
        [
            TransitionKind::Reported,
            TransitionKind::Resolved(Status::Wontfix),
            TransitionKind::Duplicate,
        ]
    );
    assert_eq!(verdict(run.convergence()), (2, 0, 0, Verdict::Converged));
}

/// `contested` behaves like `open`: it blocks convergence and escalates on a higher-severity
/// re-report without losing the contested status.
#[test]
fn a_contested_claim_escalates_and_keeps_blocking() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.resolve(
        Status::Contested,
        "author disputes: claims the tombstone is written first",
    );
    run.next_round();
    run.report(
        "cross-r2",
        Severity::Blocker,
        "r2: stale reads are user-visible",
    );

    let finding = run.finding();
    assert_eq!(finding.status, Status::Contested);
    assert_eq!(finding.severity, Severity::Blocker);
    assert_eq!((finding.news_round, finding.last_seen_round), (2, 2));
    assert_eq!(finding.source, "cross-r2");
    assert_eq!(finding.body, "r2: stale reads are user-visible");
    assert_eq!(
        finding.current_note(),
        Some("escalated: re-reported as blocker by cross-r2 in round 2")
    );
    assert_eq!(
        run.kinds(),
        [
            TransitionKind::Reported,
            TransitionKind::Resolved(Status::Contested),
            TransitionKind::Escalated,
        ]
    );
    assert_eq!(verdict(run.convergence()), (2, 1, 1, Verdict::NotConverged));
}

/// A fix recorded in the Round that reported the Finding does not converge: the fix has not
/// survived a review yet. The next Round without gate-severity news does.
#[test]
fn a_fix_in_the_reporting_round_needs_a_clean_round_to_converge() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Major, "r1 evidence");
    run.resolve(Status::Fixed, "flush on drop");
    assert_eq!(verdict(run.convergence()), (1, 0, 1, Verdict::NotConverged));

    run.next_round();
    assert_eq!(verdict(run.convergence()), (2, 0, 0, Verdict::Converged));
    let finding = run.finding();
    assert_eq!(finding.status, Status::Fixed);
    assert_eq!((finding.news_round, finding.last_seen_round), (1, 1));
    assert_eq!(finding.current_note(), Some("flush on drop"));
}

/// An open gate-severity Finding at the Round cap is a third verdict, never a pass.
#[test]
fn an_open_blocker_at_the_round_cap_is_exhausted_not_a_pass() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Blocker, "r1 evidence");
    run.next_round();
    assert_eq!(verdict(run.convergence()), (2, 1, 0, Verdict::NotConverged));
    run.next_round();

    assert_eq!(verdict(run.convergence()), (3, 1, 0, Verdict::Exhausted));
    assert_eq!(run.finding().status, Status::Open);
}

/// An open Finding below the gate neither blocks nor counts as news: the Campaign converges
/// with it still open.
#[test]
fn an_open_finding_below_the_gate_never_blocks() {
    let mut run = Run::new();
    run.report("deep-r1", Severity::Minor, "r1 evidence");

    assert_eq!(verdict(run.convergence()), (1, 0, 0, Verdict::Converged));
    let finding = run.finding();
    assert_eq!(finding.status, Status::Open);
    assert_eq!(finding.severity, Severity::Minor);
}
