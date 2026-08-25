use review_core::{
    ChangeSetV1, EventType, FindingReport, Location, PathRenameV1, RoundStartedPayloadV1, RunEvent,
    Severity, SubjectV1,
};
use review_store::{
    Cas, ConvergencePolicy, Ledger, ReportScope, ScopeAuthorityKind, Status, Verdict,
};

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
    apply_diff_round_with_renames(ledger, cas, round, paths, Vec::new());
}

fn apply_diff_round_with_renames(
    ledger: &mut Ledger,
    cas: &Cas,
    round: u32,
    paths: &[&str],
    renames: Vec<PathRenameV1>,
) {
    let base = digest('a');
    let head = digest('b');
    let change_set = ChangeSetV1::new(
        &base,
        &head,
        paths.iter().map(|path| (*path).to_string()).collect(),
        renames,
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

#[test]
fn rename_endpoints_are_in_scope_and_replay_preserves_the_existing_key() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round_with_renames(
        &mut ledger,
        &cas,
        1,
        &["src/new.rs", "src/old.rs"],
        vec![PathRenameV1 {
            old_path: "src/old.rs".into(),
            new_path: "src/new.rs".into(),
            similarity: 100,
        }],
    );
    apply_report(
        &mut ledger,
        &cas,
        "stable-legacy-key",
        1,
        Severity::Major,
        "src/old.rs",
    );
    apply_report(
        &mut ledger,
        &cas,
        "stable-legacy-key",
        1,
        Severity::Major,
        "src/new.rs",
    );
    apply_report(
        &mut ledger,
        &cas,
        "outside",
        1,
        Severity::Major,
        "src/other.rs",
    );

    assert_eq!(ledger.len(), 2);
    let renamed = ledger.get("stable-legacy-key").unwrap();
    assert_eq!(renamed.key, "stable-legacy-key");
    assert_eq!(renamed.reports.len(), 2);
    assert!(
        renamed
            .reports
            .iter()
            .all(|report| report.scope == Some(ReportScope::In))
    );
    assert_eq!(renamed.convergence_scope, Some(ReportScope::In));
    assert_eq!(
        ledger.get("outside").unwrap().convergence_scope,
        Some(ReportScope::Out)
    );
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 1);
}

#[test]
fn report_scope_accepts_raw_percent_paths_and_encoded_non_utf8_paths() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["docs/50%25-off.md", "src/a%FF.rs"]);
    apply_report(
        &mut ledger,
        &cas,
        "percent",
        1,
        Severity::Major,
        "docs/50%-off.md",
    );
    apply_report(
        &mut ledger,
        &cas,
        "non-utf8",
        1,
        Severity::Major,
        "src/a%FF.rs",
    );

    assert_eq!(
        ledger.get("percent").unwrap().convergence_scope,
        Some(ReportScope::In)
    );
    assert_eq!(
        ledger.get("non-utf8").unwrap().convergence_scope,
        Some(ReportScope::In)
    );
}

#[test]
fn any_matching_location_makes_a_typed_report_in_scope() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round_with_renames(
        &mut ledger,
        &cas,
        1,
        &["src/new.rs", "src/old.rs"],
        vec![PathRenameV1 {
            old_path: "src/old.rs".into(),
            new_path: "src/new.rs".into(),
            similarity: 100,
        }],
    );
    let report = FindingReport {
        title: "multi-location claim".into(),
        severity: Severity::Major,
        locations: vec![
            Location::at("src/a-untouched.rs", 11),
            Location::at("src/new.rs", 22),
        ],
        body: "body".into(),
        fix: "fix".into(),
        confidence: 0.9,
        failure_trace: None,
        rule_id: None,
        occurrence_key: None,
        relations: Vec::new(),
    };
    let report_id = cas
        .put_json(&serde_json::to_value(report).unwrap())
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "multi",
                    "round": 1,
                    "source": "typed",
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            &cas,
        )
        .unwrap();

    let finding = ledger.get("multi").unwrap();
    assert_eq!(finding.identity_file, "src/a-untouched.rs");
    assert_eq!(finding.identity_line, Some(11));
    assert_eq!(finding.file, "src/new.rs");
    assert_eq!(finding.line, Some(22));
    assert_eq!(finding.reports[0].scope, Some(ReportScope::In));
    assert_eq!(finding.reports[0].file, "src/new.rs");
    assert_eq!(finding.convergence_scope, Some(ReportScope::In));
}

#[test]
fn an_invalid_typed_report_is_diagnostic_unknown_instead_of_bricking_replay() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/in.rs"]);
    let report_id = cas
        .put_json(&serde_json::json!({
            "title": "bad path spelling",
            "severity": "major",
            "locations": [{"path": "./src/in.rs"}],
            "body": "body",
            "fix": "fix",
            "confidence": 0.9
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "bad-path",
                    "round": 1,
                    "source": "typed",
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            &cas,
        )
        .unwrap();

    let finding = ledger.get("bad-path").unwrap();
    assert_eq!(finding.severity, Severity::Major);
    assert_eq!(finding.title, "bad path spelling");
    assert_eq!(finding.body, "body");
    assert_eq!(finding.fix.as_deref(), Some("fix"));
    assert!(!finding.authority_diagnostic);
    assert_eq!(finding.convergence_scope, None);
    assert_eq!(finding.reports[0].scope, None);
    assert_eq!(ledger.scope_authority_failures().len(), 1);
    assert_eq!(
        ledger.scope_authority_failures()[0].authority,
        ScopeAuthorityKind::Report
    );
    assert!(
        ledger.scope_authority_failures()[0]
            .reason
            .contains("no canonical repository-relative location")
    );
    assert_eq!(
        convergence(&ledger, Severity::Major).verdict,
        Verdict::NotConverged
    );
}

#[test]
fn typed_report_semantic_failures_are_unreadable_authority() {
    for (key, report) in [
        (
            "empty-fix",
            serde_json::json!({
                "title": "title",
                "severity": "major",
                "locations": [{"path": "src/a.rs", "line": 1}],
                "body": "body",
                "fix": "",
                "confidence": 0.9
            }),
        ),
        (
            "zero-line",
            serde_json::json!({
                "title": "title",
                "severity": "major",
                "locations": [{"path": "src/a.rs", "line": 0}],
                "body": "body",
                "fix": "fix",
                "confidence": 0.9
            }),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let mut ledger = Ledger::default();
        apply_whole_tree_round(&mut ledger, &cas, 1);
        let report_id = cas.put_json(&report).unwrap();
        ledger
            .apply_event(
                &event(
                    EventType::FindingReportedV1,
                    serde_json::json!({
                        "key": key,
                        "round": 1,
                        "source": "typed",
                        "report_id": report_id,
                    }),
                    vec![report_id],
                ),
                &cas,
            )
            .unwrap();

        assert!(ledger.get(key).unwrap().authority_diagnostic, "{key}");
        assert_eq!(ledger.scope_authority_failures().len(), 1, "{key}");
        assert_eq!(
            ledger.scope_authority_failures()[0].authority,
            ScopeAuthorityKind::Report,
            "{key}"
        );
        let summary = convergence(&ledger, Severity::Major);
        assert_eq!(summary.open_blocking, 0, "{key}");
        assert_eq!(summary.new_recent, 0, "{key}");
        assert_eq!(summary.authority_failures_recent, 1, "{key}");
    }
}

#[test]
fn an_active_unreadable_report_blocks_after_its_original_clean_window() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    let report_id = cas
        .put_json(&serde_json::json!({
            "severity": "major", "locations": [], "body": "body",
            "fix": "fix", "confidence": 0.9
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "unreadable", "round": 1, "source": "typed",
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            &cas,
        )
        .unwrap();
    apply_whole_tree_round(&mut ledger, &cas, 2);

    let summary = ledger.convergence(ConvergencePolicy {
        clean_rounds: 1,
        max_rounds: 4,
        gate: Severity::Major,
    });
    assert_eq!(summary.open_blocking, 0);
    assert_eq!(summary.new_recent, 0);
    assert_eq!(summary.authority_failures_recent, 1);
    assert_eq!(summary.verdict, Verdict::NotConverged);
}

#[test]
fn an_unreadable_rereport_of_a_fixed_claim_never_ages_out() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    apply_report(&mut ledger, &cas, "claim", 1, Severity::Major, "src/a.rs");
    apply_resolution(&mut ledger, &cas, "claim", 1, Status::Fixed);

    apply_whole_tree_round(&mut ledger, &cas, 2);
    let unreadable_id = cas
        .put_json(&serde_json::json!({
            "severity": "major", "locations": [], "body": "body",
            "fix": "fix", "confidence": 0.9
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "claim", "round": 2, "source": "typed",
                    "report_id": unreadable_id,
                }),
                vec![unreadable_id],
            ),
            &cas,
        )
        .unwrap();
    apply_whole_tree_round(&mut ledger, &cas, 3);

    let finding = ledger.get("claim").unwrap();
    assert_eq!(finding.status, Status::Fixed);
    assert_eq!(finding.unreadable_reports.len(), 1);
    let summary = ledger.convergence(ConvergencePolicy {
        clean_rounds: 1,
        max_rounds: 4,
        gate: Severity::Major,
    });
    assert_eq!(summary.open_blocking, 0);
    assert_eq!(summary.new_recent, 0);
    assert_eq!(summary.authority_failures_recent, 1);
    assert_eq!(summary.verdict, Verdict::NotConverged);
}

#[test]
fn a_readable_report_replaces_an_unreadable_first_report() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);

    let unreadable_id = cas
        .put_json(&serde_json::json!({
            "severity": "blocker",
            "locations": [{"path": "src/a.rs"}],
            "body": "invalid body",
            "fix": "invalid fix",
            "confidence": 1.0
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "claim",
                    "round": 1,
                    "source": "broken",
                    "report_id": unreadable_id,
                }),
                vec![unreadable_id],
            ),
            &cas,
        )
        .unwrap();
    assert!(ledger.get("claim").unwrap().authority_diagnostic);
    assert_eq!(ledger.get("claim").unwrap().identity_file, "");

    apply_report(&mut ledger, &cas, "claim", 1, Severity::Major, "src/a.rs");
    let finding = ledger.get("claim").unwrap();
    assert!(!finding.authority_diagnostic);
    assert_eq!(finding.severity, Severity::Major);
    assert_eq!(finding.title, "claim");
    assert_eq!(finding.body, "body");
    assert_eq!(finding.convergence_severity, Some(Severity::Major));
}

#[test]
fn authority_recovery_reopens_a_placeholder_resolution_and_restores_identity() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    let unreadable_id = cas
        .put_json(&serde_json::json!({
            "severity": "blocker",
            "locations": [{"path": "src/a.rs"}],
            "body": "invalid body",
            "fix": "invalid fix",
            "confidence": 1.0
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "claim",
                    "round": 1,
                    "source": "broken",
                    "report_id": unreadable_id,
                }),
                vec![unreadable_id],
            ),
            &cas,
        )
        .unwrap();
    apply_resolution(&mut ledger, &cas, "claim", 1, Status::Wontfix);

    apply_report(&mut ledger, &cas, "claim", 1, Severity::Major, "src/a.rs");

    let finding = ledger.get("claim").unwrap();
    assert_eq!(finding.status, Status::Open);
    assert_eq!(finding.news_round, 1);
    assert_eq!(finding.identity_file, "src/a.rs");
    assert_eq!(finding.identity_line, Some(1));
    assert!(
        finding
            .current_note()
            .unwrap()
            .contains("authority placeholder")
    );
}

#[test]
fn frozen_flat_noncanonical_paths_remain_readable_and_fail_closed_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/a.rs"]);
    for (index, path) in ["./src/a.rs", "/sandbox/src/a.rs", "src/../a.rs", "   "]
        .into_iter()
        .enumerate()
    {
        let key = format!("flat-{index}");
        apply_report(&mut ledger, &cas, &key, 1, Severity::Major, path);
        let finding = ledger.get(&key).unwrap();
        assert_eq!(finding.title, "claim");
        assert_eq!(finding.body, "body");
        assert_eq!(finding.fix.as_deref(), Some("fix"));
        assert_eq!(finding.severity, Severity::Major);
        assert_eq!(finding.file, path);
        assert_eq!(finding.convergence_scope, None);
        assert_eq!(finding.convergence_scope_label(), "unknown");
    }
    assert_eq!(convergence(&ledger, Severity::Major).open_blocking, 4);
}

#[test]
fn an_old_authority_failure_ages_out_after_the_clean_window() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_diff_round(&mut ledger, &cas, 1, &["src/a.rs"]);
    let report_id = cas
        .put_json(&serde_json::json!({
            "title": "bad path",
            "severity": "major",
            "locations": [{"path": "./src/a.rs"}],
            "body": "body",
            "fix": "fix",
            "confidence": 0.9
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "bad-path",
                    "round": 1,
                    "source": "typed",
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            &cas,
        )
        .unwrap();
    apply_resolution(&mut ledger, &cas, "bad-path", 1, Status::Fixed);
    apply_diff_round(&mut ledger, &cas, 2, &["src/a.rs"]);
    apply_diff_round(&mut ledger, &cas, 3, &["src/a.rs"]);

    assert_eq!(
        ledger
            .convergence(ConvergencePolicy {
                clean_rounds: 2,
                max_rounds: 4,
                gate: Severity::Major,
            })
            .verdict,
        Verdict::Converged
    );
}

#[test]
fn an_unreadable_later_report_does_not_overwrite_a_readable_claim() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();
    apply_whole_tree_round(&mut ledger, &cas, 1);
    apply_report(&mut ledger, &cas, "claim", 1, Severity::Major, "src/a.rs");
    apply_resolution(&mut ledger, &cas, "claim", 1, Status::Fixed);
    apply_whole_tree_round(&mut ledger, &cas, 2);

    let report_id = cas
        .put_json(&serde_json::json!({
            "severity": "blocker",
            "locations": [{"path": "./src/a.rs"}],
            "body": "replacement body",
            "fix": "replacement fix",
            "confidence": 1.0
        }))
        .unwrap();
    ledger
        .apply_event(
            &event(
                EventType::FindingReportedV1,
                serde_json::json!({
                    "key": "claim",
                    "round": 2,
                    "source": "typed",
                    "report_id": report_id,
                }),
                vec![report_id],
            ),
            &cas,
        )
        .unwrap();

    let finding = ledger.get("claim").unwrap();
    assert_eq!(finding.status, Status::Fixed);
    assert_eq!(finding.severity, Severity::Major);
    assert_eq!(finding.title, "claim");
    assert_eq!(finding.body, "body");
    assert_eq!(finding.fix.as_deref(), Some("fix"));
    assert_eq!(finding.reports.len(), 2);
    assert_eq!(finding.reports[1].scope, None);
    assert_eq!(
        convergence(&ledger, Severity::Major).verdict,
        Verdict::NotConverged
    );
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
    assert_eq!(
        convergence(&ledger, Severity::Major).authority_failures_recent,
        1
    );
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
fn repeated_round_subject_resolution_reverifies_artifact_authority() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let mut ledger = Ledger::default();

    let change_set = ChangeSetV1::new(
        digest('a'),
        digest('b'),
        vec!["src/in.rs".into()],
        vec![],
        b"patch",
        "git test",
        "policy test",
    )
    .unwrap();
    let change_set_id = cas
        .put_json(&serde_json::to_value(change_set).unwrap())
        .unwrap();
    let subject = SubjectV1::diff(digest('b'), digest('a'), change_set_id);
    let subject_id = cas
        .put_json(&serde_json::to_value(subject).unwrap())
        .unwrap();
    let start = |round| {
        event(
            EventType::RoundStartedV1,
            serde_json::to_value(RoundStartedPayloadV1 {
                round,
                epoch: 1,
                campaign_manifest_id: digest('c'),
                subject_id: subject_id.clone(),
                prior_finding_set_id: digest('d'),
                prior_demand_set_id: digest('e'),
            })
            .unwrap(),
            vec![],
        )
    };
    ledger.apply_event(&start(1), &cas).unwrap();

    let hex = subject_id.strip_prefix("sha256:").unwrap();
    std::fs::write(
        dir.path().join("objects").join(&hex[..2]).join(&hex[2..]),
        b"corrupt after verified resolution",
    )
    .unwrap();
    ledger.apply_event(&start(2), &cas).unwrap();
    ledger.round = 2;
    apply_report(&mut ledger, &cas, "cached", 2, Severity::Major, "src/in.rs");

    assert_eq!(ledger.get("cached").unwrap().convergence_scope, None);
    assert_eq!(ledger.scope_authority_failures().len(), 1);
    assert_eq!(
        ledger.scope_authority_failures()[0].authority,
        ScopeAuthorityKind::Subject
    );
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
    assert_eq!(ledger.scope_authority_failures().len(), 1);
    assert_eq!(
        ledger.scope_authority_failures()[0].authority,
        ScopeAuthorityKind::RoundBinding
    );
    assert!(
        ledger.scope_authority_failures()[0]
            .reason
            .contains("Report round 2 disagrees")
    );
}
