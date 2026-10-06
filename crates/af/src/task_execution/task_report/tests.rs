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
            "| implement-x | implement | fixture/plain@1.0.0 | pass | 0 | 2 (1 failed) | 1,234 | 3.2s | 2h 00m |"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "**Totals:** 2 Tasks · 0 rounds · 4 Attempts (2 failed) · 2,468 tokens · 6.5s active"
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
        text.contains("| root.nodes.implement | implement | codex gpt-6-sol/high | 2 (1 failed) | 1,234 | 1m 05s | pagination passed 840ms |"),
        "{text}"
    );
}

/// The cells of a Markdown table row as GitHub splits it: a backslash escapes the character
/// after it, and every other `|` ends a cell.
fn table_cells(line: &str) -> Vec<String> {
    let inner = line
        .trim()
        .strip_prefix('|')
        .and_then(|l| l.strip_suffix('|'))
        .unwrap();
    let (mut cells, mut cell, mut chars) = (Vec::new(), String::new(), inner.chars());
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cell.push(c);
                cell.extend(chars.next());
            }
            '|' => cells.push(std::mem::take(&mut cell)),
            _ => cell.push(c),
        }
    }
    cells.push(cell);
    cells
}

#[test]
fn recorded_text_cannot_break_a_cell_or_open_markup() {
    let mut task = entry("a", "gpt-6-sol");
    task.outcome = "m|<script>\u{1b}[31m\u{202e}".into();
    let text = markdown(&TaskReportV1::new(vec![task]).unwrap());
    assert!(!text.contains("<script>"), "{text}");
    assert!(
        !text.contains('\u{1b}') && !text.contains('\u{202e}'),
        "{text}"
    );
    assert!(text.contains("| m&#124;&lt;script&gt;?[31m? |"), "{text}");
}

/// A backslash before a pipe would escape the escape and end the cell; it is encoded first.
#[test]
fn a_backslash_before_a_pipe_stays_in_its_cell() {
    let mut task = entry("a", "gpt-6-sol");
    task.outcome = "m\\|x".into();
    task.nodes.as_mut().unwrap()[0].checks.clear();
    let text = markdown(&TaskReportV1::new(vec![task]).unwrap());
    let row = text.lines().find(|line| line.starts_with("| a |")).unwrap();
    assert_eq!(table_cells(row).len(), TASK_REPORT_COLUMNS.len(), "{row}");
    assert!(row.contains("| m&#92;&#124;x |"), "{row}");
    assert_eq!(cell("m\\|x"), "m&#92;&#124;x");
    assert_eq!(cell("a\\\\|b|c"), "a&#92;&#92;&#124;b&#124;c");
}

#[test]
fn token_counts_read_with_thousands_separators_in_markdown_only() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(205_295), "205,295");
    assert_eq!(thousands(1_234_567), "1,234,567");
    assert_eq!(
        thousands(u128::MAX),
        "340,282,366,920,938,463,463,374,607,431,768,211,455"
    );
    let mut task = entry("a", "gpt-6-sol");
    task.chargeable_tokens = DecimalU128::from(205_295);
    task.nodes.as_mut().unwrap()[0].tokens = DecimalU128::from(205_295);
    task.attempts.as_mut().unwrap().failed_tokens = DecimalU128::from(12_345);
    task.attempts.as_mut().unwrap().failures[0].tokens = DecimalU128::from(12_345);
    let report = TaskReportV1::new(vec![task]).unwrap();
    let text = markdown(&report);
    assert!(text.contains("| 2 (1 failed) | 205,295 | 3.2s |"), "{text}");
    assert!(text.contains("· 205,295 tokens ·"), "{text}");
    assert!(
        text.contains("(1 provider_failure; 12,345 tokens)"),
        "{text}"
    );
    assert!(
        text.contains("| 2 (1 failed) | 205,295 | 1m 05s |"),
        "{text}"
    );
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["tasks"][0]["chargeable_tokens"], "205295");
    assert_eq!(json["totals"]["chargeable_tokens"], "205295");
}

#[test]
fn only_a_model_identity_is_copied_into_the_report() {
    assert_eq!(report_model("gpt-6-sol"), "gpt-6-sol");
    assert_eq!(report_model("anthropic/claude-3.5"), "anthropic/claude-3.5");
    for private in [
        "/Users/fixture/.codex/auth.json",
        "~/.claude",
        "home/fixture/model",
        "fixture/.af/state",
        "codex-personal fixture@example.invalid",
        "m\\|x",
    ] {
        assert_eq!(
            report_model(private),
            TASK_REPORT_UNKNOWN_MODEL,
            "{private}"
        );
    }
    let text =
        markdown(&TaskReportV1::new(vec![entry("a", &report_model("/home/x/.codex"))]).unwrap());
    assert!(text.contains("| codex unknown/high |"), "{text}");
}

/// `af task start --execute` (epoch 1) is interrupted with an Attempt pending, `af task
/// refresh` (epoch 2) settles that Attempt as abandoned and refreshes the source, and `af task
/// run` (epoch 3) finishes. The refresh is not a run and its lease time is not active time.
#[test]
fn a_refresh_that_recovers_a_pending_attempt_is_not_a_run() {
    let events = [
        (1, 1_000, Mark::Other),
        (1, 1_010, Mark::Work),
        (1, 1_400, Mark::Work),
        (2, 9_000, Mark::Other),
        (2, 9_050, Mark::Other),
        (2, 9_900, Mark::Refresh),
        (2, 9_950, Mark::Other),
        (3, 20_000, Mark::Other),
        (3, 20_010, Mark::Other),
        (3, 20_700, Mark::Work),
        (3, 21_000, Mark::Other),
    ];
    assert_eq!(
        tally_runs(&events),
        Some(EventTimes {
            runs: 2,
            wall_ms: 20_000,
            active_ms: 400 + 1_000,
        })
    );
    // A command that only recovered earlier Attempts and stopped executed nothing either.
    let recovered_only = [
        (1, 1_000, Mark::Work),
        (2, 5_000, Mark::Other),
        (2, 5_500, Mark::Other),
    ];
    assert_eq!(
        tally_runs(&recovered_only),
        Some(EventTimes {
            runs: 1,
            wall_ms: 4_500,
            active_ms: 0,
        })
    );
    assert_eq!(tally_runs(&[]), None);
}

/// One node ran an Attempt under the plan before `af task refresh` and two under the plan
/// after it, which binds its slot to another Worker: each Worker gets a row with only its own
/// Attempts, failures and tokens.
#[test]
fn a_node_refreshed_onto_another_worker_charges_each_worker_its_own_attempts() {
    let model = |provider_kind: &str, model: &str| {
        Some(TaskReportWorkerV1::Model {
            provider_kind: provider_kind.into(),
            model: model.into(),
            effort: "high".into(),
        })
    };
    let attempt = |node: &str, worker, failure, tokens| CountedAttempt {
        node: node.into(),
        role: "implement".into(),
        worker,
        failure,
        tokens,
        elapsed_ms: Some(1_000),
        checks: Vec::new(),
    };
    let (summary, nodes) = tally_attempts(vec![
        attempt(
            "root.nodes.implement",
            model("codex", "gpt-6-sol"),
            Some(TaskReportFailureClassV1::Abandoned),
            205_295,
        ),
        attempt("root.nodes.check", None, None, 0),
        attempt(
            "root.nodes.implement",
            model("claude", "claude-opus-5-5"),
            Some(TaskReportFailureClassV1::ProviderFailure),
            7,
        ),
        attempt(
            "root.nodes.implement",
            model("claude", "claude-opus-5-5"),
            None,
            40_000,
        ),
    ])
    .unwrap();
    assert_eq!(summary.failed, 2);
    assert_eq!(summary.failed_tokens.get(), 205_302);
    let rows: Vec<_> = nodes
        .iter()
        .map(|n| {
            (
                n.node.as_str(),
                worker_label(n.worker.as_ref()),
                n.attempts,
                n.failed_attempts,
                n.tokens.get(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "root.nodes.implement",
                "codex gpt-6-sol/high".to_owned(),
                1,
                1,
                205_295
            ),
            ("root.nodes.check", "-".to_owned(), 1, 0, 0),
            (
                "root.nodes.implement",
                "claude claude-opus-5-5/high".to_owned(),
                2,
                1,
                40_007
            ),
        ]
    );
    let mut task = entry("a", "gpt-6-sol");
    task.attempts = Some(TaskReportAttemptsV1 {
        total: 4,
        ..summary
    });
    task.nodes = Some(nodes);
    let text = markdown(&TaskReportV1::new(vec![task]).unwrap());
    assert!(
        text.contains(
            "| root.nodes.implement | implement | codex gpt-6-sol/high | 1 (1 failed) | 205,295 |"
        ),
        "{text}"
    );
    assert!(
        text.contains("| root.nodes.implement | implement | claude claude-opus-5-5/high | 2 (1 failed) | 40,007 |"),
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
            "| old | implement | unknown | pass (collected) | unknown | unknown | 1,234 |"
        ),
        "{text}"
    );
    assert!(
        text.contains("**Totals:** 2 Tasks · unknown rounds · unknown Attempts · 2,468 tokens"),
        "{text}"
    );
    assert!(
        text.contains("The Task was collected: its Attempts are no longer recorded."),
        "{text}"
    );
    assert!(
        text.contains("| 1,234 | unknown | pagination passed unknown |"),
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
