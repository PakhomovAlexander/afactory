use super::*;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn node(name: &str, worker: Option<TaskReportWorkerV1>) -> TaskReportNodeV1 {
    TaskReportNodeV1 {
        node: name.into(),
        role: "implement".into(),
        worker,
        attempts: 2,
        failed_attempts: 1,
        tokens: DecimalU128::from(1234),
        elapsed_ms: Some(65_000),
        checks: vec![TaskReportCheckV1 {
            name: "pagination".into(),
            status: TaskReportCheckStatusV1::Passed,
            elapsed_ms: Some(840),
        }],
    }
}

fn entry(task_id: &str, model: &str) -> TaskReportEntryV1 {
    TaskReportEntryV1 {
        task_id: task_id.into(),
        kind: "implement".into(),
        pipeline: Some("fixture/plain@1.0.0".into()),
        outcome: "pass".into(),
        collected: false,
        review_rounds: Some(0),
        runs: 2,
        attempts: Some(TaskReportAttemptsV1 {
            total: 2,
            failed: 1,
            failed_tokens: DecimalU128::from(34),
            failures: vec![TaskReportFailureV1 {
                class: TaskReportFailureClassV1::ProviderFailure,
                attempts: 1,
                tokens: DecimalU128::from(34),
            }],
        }),
        chargeable_tokens: DecimalU128::from(1234),
        wall_ms: 7_200_000,
        active_ms: 3_250,
        nodes: Some(vec![node(
            "root.nodes.implement",
            Some(TaskReportWorkerV1::Model {
                provider_kind: "codex".into(),
                model: model.into(),
                effort: "high".into(),
            }),
        )]),
    }
}

#[test]
fn durations_read_as_a_person_reads_them_and_never_round_up() {
    assert_eq!(duration(0), "0ms");
    assert_eq!(duration(999), "999ms");
    assert_eq!(duration(1_000), "1.0s");
    assert_eq!(duration(3_299), "3.2s");
    assert_eq!(duration(59_999), "59.9s");
    assert_eq!(duration(60_000), "1m 00s");
    assert_eq!(duration(245_999), "4m 05s");
    assert_eq!(duration(3_600_000), "1h 00m");
    assert_eq!(duration(3_720_000), "1h 02m");
}

#[test]
fn the_block_has_its_markers_columns_totals_and_one_details_element_per_task() {
    let report = TaskReportV1::new(vec![
        entry("implement-x", "gpt-6-sol"),
        entry("verify-x", "gpt-6-sol"),
    ])
    .unwrap();
    let text = markdown(&report);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first(), Some(&TASK_REPORT_BEGIN));
    assert_eq!(lines.last(), Some(&TASK_REPORT_END));
    assert_eq!(
        lines[3],
        "| Task | Kind | Pipeline | Outcome | Rounds | Attempts | Tokens | Active time | Wall time |"
    );
    assert!(
        text.contains(
            "| implement-x | implement | fixture/plain@1.0.0 | pass | 0 | 2 (1 failed) | 1234 | 3.2s | 2h 00m |"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "**Totals:** 2 Tasks · 0 rounds · 4 Attempts (2 failed) · 2468 tokens · 6.5s active"
        ),
        "{text}"
    );
    assert_eq!(text.matches("<details>").count(), 2);
    assert!(
        text.contains(
            "<summary>implement-x: 2 runs, 1 failed Attempt (1 provider_failure; 34 tokens)</summary>"
        ),
        "{text}"
    );
    assert!(
        text.contains("| root.nodes.implement | implement | codex gpt-6-sol/high | 2 (1 failed) | 1234 | 1m 05s | pagination passed 840ms |"),
        "{text}"
    );
}

#[test]
fn recorded_text_cannot_break_a_cell_or_open_markup() {
    let report = TaskReportV1::new(vec![entry("a", "m|<script>\u{1b}[31m\u{202e}")]).unwrap();
    let text = markdown(&report);
    assert!(!text.contains("<script>"), "{text}");
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{202e}'),
        "{text}"
    );
    assert!(
        text.contains("codex m\\|&lt;script&gt;?[31m?/high"),
        "{text}"
    );
}

#[test]
fn unknown_figures_are_written_as_unknown_never_as_zero() {
    let mut collected = entry("old", "gpt-6-sol");
    collected.collected = true;
    collected.attempts = None;
    collected.nodes = None;
    collected.review_rounds = None;
    collected.pipeline = None;
    let mut untimed = entry("new", "gpt-6-sol");
    untimed.nodes.as_mut().unwrap()[0].elapsed_ms = None;
    untimed.nodes.as_mut().unwrap()[0].checks[0].elapsed_ms = None;
    let text = markdown(&TaskReportV1::new(vec![collected, untimed]).unwrap());
    assert!(
        text.contains(
            "| old | implement | unknown | pass (collected) | unknown | unknown | 1234 |"
        ),
        "{text}"
    );
    assert!(
        text.contains("**Totals:** 2 Tasks · unknown rounds · unknown Attempts · 2468 tokens"),
        "{text}"
    );
    assert!(
        text.contains("The Task was collected: its Attempts are no longer recorded."),
        "{text}"
    );
    assert!(
        text.contains("| 1234 | unknown | pagination passed unknown |"),
        "{text}"
    );
}

/// The checked-in fixture pair `scripts/test-check-pr-report.py` checks: the Markdown is this
/// renderer's output for the JSON document beside it, which `af task report --json` printed for
/// a test Store. A renderer change that the fixture does not follow fails here, and a checker
/// change that refuses the renderer's block fails there.
#[test]
fn the_checked_in_fixture_is_what_the_renderer_prints_for_its_document() {
    let directory = workspace().join("fixtures/task-report");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("report.json")).unwrap()).unwrap();
    let report: TaskReportV1 = serde_json::from_value(document.clone()).unwrap();
    report.validate().unwrap();
    assert_eq!(serde_json::to_value(&report).unwrap(), document);
    assert_eq!(
        markdown(&report),
        std::fs::read_to_string(directory.join("report.md")).unwrap(),
        "regenerate fixtures/task-report/report.md from report.json (see its README)"
    );
}
