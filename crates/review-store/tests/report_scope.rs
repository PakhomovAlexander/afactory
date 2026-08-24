use review_core::{ChangeSetV1, EventType, RoundStartedPayloadV1, RunEvent, Severity, SubjectV1};
use review_store::{Cas, ConvergencePolicy, Ledger, ReportScope, Status, Verdict};

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
        occurred_at: "2026-08-24T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs,
        payload,
    }
}

fn apply_diff_round(ledger: &mut Ledger, cas: &Cas, round: u32, paths: &[&str]) {
    let base = digest('a');
    let head = digest('b');
    let change_set = ChangeSetV1::new(
        &base,
        &head,
        paths.iter().map(|path| (*path).to_string()).collect(),
        Vec::new(),
        b"",
        "git version test",
        "test-policy-v1",
    )
    .unwrap();
    let change_set_id = cas
        .put_json(&serde_json::to_value(change_set).unwrap())
        .unwrap();
    let subject = SubjectV1::diff(head, base, &change_set_id);
    let subject_id = cas
        .put_json(&serde_json::to_value(subject).unwrap())
        .unwrap();
    apply_round(ledger, cas, round, subject_id);
}

fn apply_whole_tree_round(ledger: &mut Ledger, cas: &Cas, round: u32) {
    let subject = SubjectV1::whole_tree(digest('c'));
    let subject_id = cas
        .put_json(&serde_json::to_value(subject).unwrap())
        .unwrap();
    apply_round(ledger, cas, round, subject_id);
}

fn apply_round(ledger: &mut Ledger, cas: &Cas, round: u32, subject_id: String) {
    let payload = RoundStartedPayloadV1 {
        round,
        epoch: 1,
        campaign_manifest_id: digest('d'),
        subject_id,
        prior_finding_set_id: digest('e'),
        prior_demand_set_id: digest('f'),
    };
    ledger
        .apply_event(
            &event(
                EventType::RoundStartedV1,
                serde_json::to_value(payload).unwrap(),
                Vec::new(),
            ),
            cas,
        )
        .unwrap();
    // RoundStarted supplies Scope authority; GenerationAdvanced owns the Ledger counter.
    ledger.round = round;
}

fn apply_report(
    ledger: &mut Ledger,
    cas: &Cas,
    key: &str,
    round: u32,
    severity: Severity,
    file: &str,
) {
    let severity = match severity {
        Severity::Minor => "minor",
        Severity::Major => "major",
        Severity::Blocker => "blocker",
    };
    let report_id = cas
        .put_json(&serde_json::json!({
            "title": "claim",
            "severity": severity,
            "file": file,
            "line": 1,
            "body": "body",
            "fix": "fix",
            "confidence": 0.9,
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": key,
                    "round": round,
                    "source": format!("reviewer-{file}"),
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            cas,
        )
        .unwrap();
}

fn apply_legacy_report(ledger: &mut Ledger, cas: &Cas) {
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "legacy",
                    "round": 1,
                    "source": "legacy",
                    "severity": "major",
                    "file": "src/legacy.rs",
                    "line": 1,
                    "title": "legacy claim",
                    "body": "body",
                    "confidence": 0.5,
                    "imported": true,
                }),
                Vec::new(),
            ),
            cas,
        )
        .unwrap();
}

fn apply_resolution(ledger: &mut Ledger, cas: &Cas, key: &str, round: u32, status: Status) {
    ledger
        .apply_event(
            &event(
                EventType::FindingResolvedV1,
                serde_json::json!({
                    "key": key,
                    "status": status.as_str(),
                    "round": round,
                }),
                Vec::new(),
            ),
            cas,
        )
        .unwrap();
}

fn convergence(ledger: &Ledger, gate: Severity) -> review_store::Convergence {
    ledger.convergence(ConvergencePolicy {
        clean_rounds: 1,
        max_rounds: 3,
        gate,
    })
}

#[test]
fn mixed_scope_uses_only_the_highest_non_out_claim_severity() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/other.rs"]);
    apply_report(
        &mut ledger,
        &cas,
        "claim",
        1,
        Severity::Blocker,
        "src/shared.rs",
    );
    apply_diff_round(&mut ledger, &cas, 2, &["src/shared.rs"]);
    apply_report(
        &mut ledger,
        &cas,
        "claim",
        2,
        Severity::Major,
        "src/shared.rs",
    );

    let finding = ledger.get("claim").unwrap();
    assert_eq!(finding.severity, Severity::Blocker);
    assert_eq!(finding.reports[0].scope, Some(ReportScope::Out));
    assert_eq!(finding.reports[1].scope, Some(ReportScope::In));
    assert_eq!(finding.convergence_scope, Some(ReportScope::In));
    assert_eq!(finding.convergence_severity, Some(Severity::Major));
    assert_eq!(
        convergence(&ledger, Severity::Blocker).verdict,
        Verdict::Converged
    );
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 1);
}

#[test]
fn wholly_out_findings_do_not_block_or_extend_the_clean_window() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/in.rs"]);
    apply_report(&mut ledger, &cas, "out", 1, Severity::Blocker, "src/out.rs");

    let result = convergence(&ledger, Severity::Major);
    assert_eq!(result.open_blocking, 0);
    assert_eq!(result.new_recent, 0);
    assert_eq!(result.verdict, Verdict::Converged);
}

#[test]
fn whole_tree_and_change_wide_reports_are_in_scope() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut whole_tree = Ledger::default();
    apply_whole_tree_round(&mut whole_tree, &cas, 1);
    apply_report(
        &mut whole_tree,
        &cas,
        "whole",
        1,
        Severity::Major,
        "src/untouched.rs",
    );
    assert_eq!(
        whole_tree.get("whole").unwrap().reports[0].scope,
        Some(ReportScope::In)
    );

    let mut change_wide = Ledger::default();
    apply_diff_round(&mut change_wide, &cas, 1, &["src/in.rs"]);
    apply_report(&mut change_wide, &cas, "wide", 1, Severity::Major, "");
    assert_eq!(
        change_wide.get("wide").unwrap().reports[0].scope,
        Some(ReportScope::In)
    );
}

#[test]
fn legacy_unknown_is_presentation_only_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    apply_legacy_report(&mut ledger, &cas);
    ledger.round = 1;

    let finding = ledger.get("legacy").unwrap();
    assert_eq!(finding.status, Status::Open);
    assert_eq!(finding.reports[0].scope, None);
    assert_eq!(finding.reports[0].scope_label(), "unknown");
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 1);
}

#[test]
fn unavailable_subject_is_diagnostic_unknown_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_round(&mut ledger, &cas, 1, digest('9'));
    apply_report(
        &mut ledger,
        &cas,
        "unavailable",
        1,
        Severity::Major,
        "src/main.rs",
    );

    let finding = ledger.get("unavailable").unwrap();
    assert_eq!(finding.reports[0].scope, None);
    assert_eq!(finding.convergence_scope_label(), "unknown");
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 1);
    assert_eq!(ledger.scope_authority_failures().len(), 1);
    assert_eq!(ledger.scope_authority_failures()[0].round, 1);
}

#[test]
fn a_later_round_does_not_rewrite_an_earlier_report_scope() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/first.rs"]);
    apply_report(
        &mut ledger,
        &cas,
        "stale",
        1,
        Severity::Major,
        "src/first.rs",
    );
    apply_diff_round(&mut ledger, &cas, 2, &["src/second.rs"]);

    assert_eq!(
        ledger.get("stale").unwrap().reports[0].scope,
        Some(ReportScope::In)
    );
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 1);
}

#[test]
fn reopen_replaces_the_active_scoped_severity_instead_of_resurrecting_history() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    apply_report(
        &mut ledger,
        &cas,
        "downgrade",
        1,
        Severity::Blocker,
        "src/main.rs",
    );
    apply_resolution(&mut ledger, &cas, "downgrade", 1, Status::Fixed);
    apply_whole_tree_round(&mut ledger, &cas, 2);
    apply_report(
        &mut ledger,
        &cas,
        "downgrade",
        2,
        Severity::Minor,
        "src/main.rs",
    );

    let finding = ledger.get("downgrade").unwrap();
    assert_eq!(finding.severity, Severity::Minor);
    assert_eq!(finding.convergence_severity, Some(Severity::Minor));
    assert_eq!(
        convergence(&ledger, Severity::Major).verdict,
        Verdict::Converged
    );
}

#[test]
fn a_report_without_its_exact_round_subject_is_unknown_and_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    apply_report(
        &mut ledger,
        &cas,
        "mismatch",
        2,
        Severity::Major,
        "src/main.rs",
    );

    let finding = ledger.get("mismatch").unwrap();
    assert_eq!(finding.reports[0].scope, None);
    assert_eq!(finding.convergence_scope_label(), "unknown");
    assert_eq!(finding.convergence_severity, Some(Severity::Major));
}
