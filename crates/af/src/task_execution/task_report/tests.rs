use super::*;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn model(provider_kind: &str, model: &str) -> Option<TaskReportWorkerV1> {
    Some(TaskReportWorkerV1::Model {
        provider_kind: provider_kind.into(),
        model: model.into(),
        effort: "high".into(),
    })
}

fn step(
    stage: u64,
    role: &str,
    nodes: &[&str],
    worker: Option<TaskReportWorkerV1>,
    checks: &[&str],
) -> TaskReportStepV1 {
    TaskReportStepV1 {
        stage,
        role: role.into(),
        nodes: nodes.iter().map(|n| n.to_string()).collect(),
        worker,
        checks: checks.iter().map(|c| c.to_string()).collect(),
    }
}

/// `fixture/plain@1.0.0`: an implementer bound to `model`, then a gate.
fn plain(worker: &str) -> TaskReportPipelineV1 {
    TaskReportPipelineV1 {
        name: "fixture/plain".into(),
        version: "1.0.0".into(),
        steps: vec![
            step(
                1,
                "implement",
                &["root.nodes.implement"],
                model("codex", worker),
                &[],
            ),
            step(2, "gate", &["root.nodes.check"], None, &["pagination"]),
        ],
    }
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
        unknown_usage: 0,
    }
}

fn entry(task_id: &str, worker: &str) -> TaskReportEntryV1 {
    TaskReportEntryV1 {
        round: 0,
        task_id: task_id.into(),
        kind: "implement".into(),
        pipeline: Some("fixture/plain@1.0.0".into()),
        outcome: "pass".into(),
        collected: false,
        review_rounds: Some(0),
        findings: None,
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
            unknown_usage: 0,
            unknown_usage_causes: Vec::new(),
        }),
        collected_unknown_usage: 0,
        chargeable_tokens: DecimalU128::from(1234),
        wall_ms: 7_200_000,
        active_ms: 3_250,
        nodes: Some(vec![node("root.nodes.implement", model("codex", worker))]),
    }
}

/// The report over `tasks`, which all run `fixture/plain@1.0.0` with `gpt-6-sol`.
fn report(tasks: Vec<TaskReportEntryV1>) -> TaskReportV1 {
    TaskReportV1::new(vec![plain("gpt-6-sol")], tasks).unwrap()
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

/// One Task of the five below: one node, whose Attempts are the Task's.
#[allow(clippy::too_many_arguments)]
fn round(
    task_id: &str,
    kind: &str,
    pipeline: &str,
    outcome: &str,
    findings: Option<TaskReportFindingsV1>,
    tokens: u128,
    active_ms: u64,
    wall_ms: u64,
    runs: u64,
    (total, failure): (u64, Option<(TaskReportFailureClassV1, u128)>),
    node: TaskReportNodeV1,
) -> TaskReportEntryV1 {
    TaskReportEntryV1 {
        round: 0,
        task_id: task_id.into(),
        kind: kind.into(),
        pipeline: Some(pipeline.into()),
        outcome: outcome.into(),
        collected: false,
        review_rounds: Some(u64::from(findings.is_some())),
        findings,
        runs,
        attempts: Some(TaskReportAttemptsV1 {
            total,
            failed: u64::from(failure.is_some()),
            failed_tokens: DecimalU128::from(failure.map_or(0, |(_, tokens)| tokens)),
            failures: failure
                .into_iter()
                .map(|(class, tokens)| TaskReportFailureV1 {
                    class,
                    attempts: 1,
                    tokens: DecimalU128::from(tokens),
                })
                .collect(),
            unknown_usage: 0,
            unknown_usage_causes: Vec::new(),
        }),
        collected_unknown_usage: 0,
        chargeable_tokens: DecimalU128::from(tokens),
        wall_ms,
        active_ms,
        nodes: Some(vec![node]),
    }
}

fn findings(major: u64, minor: u64, failed_reviewers: u64) -> Option<TaskReportFindingsV1> {
    Some(TaskReportFindingsV1 {
        blocker: Some(0),
        major: Some(major),
        minor: Some(minor),
        review_ran: true,
        gate_failed: false,
        failed_reviewers,
    })
}

/// The five Tasks this branch's own work ran, shaped as a test Store would record them: four
/// rounds of a reviewed implementation pipeline — six major and one minor finding, then a gate
/// that failed so the review did not run, then one major finding with a reviewer whose Attempt
/// failed, then a clean review — and a verification Task without a review. The block leads with
/// the two pipelines in first-use order, then one row per round and the totals row.
#[test]
fn five_rounds_render_their_pipelines_first_then_one_row_each_and_the_total() {
    let reviewed = TaskReportPipelineV1 {
        name: "afactory/implementation-reviewed".into(),
        version: "1.0.0".into(),
        steps: vec![
            step(
                1,
                "implement",
                &["root.nodes.implement"],
                model("codex", "gpt-6-sol"),
                &[],
            ),
            step(
                2,
                "gate",
                &["root.nodes.review.nodes.checks"],
                None,
                &["clippy", "fmt", "test"],
            ),
            step(
                3,
                "review",
                &[
                    "root.nodes.review.nodes.bugs",
                    "root.nodes.review.nodes.correctness",
                ],
                model("codex", "gpt-6-sol"),
                &[],
            ),
            step(
                4,
                "evaluate",
                &["root.nodes.evaluate"],
                model("claude", "claude-opus-5-5"),
                &[],
            ),
        ],
    };
    let verification = TaskReportPipelineV1 {
        name: "afactory/verification".into(),
        version: "1.0.0".into(),
        steps: vec![
            step(
                1,
                "gate",
                &["root.nodes.checks"],
                None,
                &["clippy", "fmt", "test"],
            ),
            step(
                2,
                "evaluate",
                &["root.nodes.evaluate"],
                model("claude", "claude-opus-5-5"),
                &[],
            ),
        ],
    };
    let one =
        |name: &str, role: &str, worker, attempts, failed, tokens, elapsed_ms| TaskReportNodeV1 {
            node: name.into(),
            role: role.into(),
            worker,
            attempts,
            failed_attempts: failed,
            tokens: DecimalU128::from(tokens),
            elapsed_ms: Some(elapsed_ms),
            checks: Vec::new(),
            unknown_usage: 0,
        };
    let implement = "root.nodes.implement";
    let gpt = || model("codex", "gpt-6-sol");
    let mut gate = one(
        "root.nodes.review.nodes.checks",
        "check",
        None,
        3,
        0,
        98_402,
        365_000,
    );
    gate.checks = vec![
        TaskReportCheckV1 {
            name: "fmt".into(),
            status: TaskReportCheckStatusV1::Passed,
            elapsed_ms: Some(2_100),
        },
        TaskReportCheckV1 {
            name: "clippy".into(),
            status: TaskReportCheckStatusV1::Failed,
            elapsed_ms: Some(130_000),
        },
    ];
    let reviewed_label = "afactory/implementation-reviewed@1.0.0";
    let tasks = vec![
        round(
            "task-report-layout",
            "implement",
            reviewed_label,
            "changes_requested",
            findings(6, 1, 0),
            412_907,
            1_421_000,
            3_720_000,
            2,
            (6, Some((TaskReportFailureClassV1::ProviderFailure, 3_120))),
            one(implement, "implement", gpt(), 6, 1, 412_907, 1_380_000),
        ),
        round(
            "task-report-layout-2",
            "implement",
            reviewed_label,
            "changes_requested",
            Some(TaskReportFindingsV1 {
                blocker: Some(0),
                major: Some(0),
                minor: Some(0),
                review_ran: false,
                gate_failed: true,
                failed_reviewers: 0,
            }),
            98_402,
            365_000,
            400_000,
            1,
            (3, None),
            gate,
        ),
        round(
            "task-report-layout-3",
            "implement",
            reviewed_label,
            "changes_requested",
            findings(1, 0, 1),
            233_018,
            912_000,
            1_000_000,
            1,
            (6, Some((TaskReportFailureClassV1::ProcessFailure, 0))),
            one(
                "root.nodes.review.nodes.bugs",
                "review",
                gpt(),
                6,
                1,
                233_018,
                900_000,
            ),
        ),
        round(
            "task-report-layout-4",
            "implement",
            reviewed_label,
            "pass",
            findings(0, 0, 0),
            201_555,
            750_000,
            800_000,
            1,
            (5, None),
            one(implement, "implement", gpt(), 5, 0, 201_555, 700_000),
        ),
        round(
            "verify-task-report-layout",
            "verify",
            "afactory/verification@1.0.0",
            "verified",
            None,
            41_230,
            242_000,
            250_000,
            1,
            (2, None),
            one(
                "root.nodes.evaluate",
                "evaluate",
                model("claude", "claude-opus-5-5"),
                2,
                0,
                41_230,
                240_000,
            ),
        ),
    ];
    let report = TaskReportV1::new(vec![reviewed, verification], tasks).unwrap();
    let expected = "\
<!-- af-task-report:v1 -->
### af task report

**afactory/implementation-reviewed@1.0.0**: implement (codex gpt-6-sol/high) → gate (clippy, fmt, test) → review (bugs, correctness: codex gpt-6-sol/high) → evaluate (claude claude-opus-5-5/high)

**afactory/verification@1.0.0**: gate (clippy, fmt, test) → evaluate (claude claude-opus-5-5/high)

| Round | Task | Outcome | Findings | Tokens | Active |
| ---: | --- | --- | --- | ---: | ---: |
| 1 | task-report-layout | changes_requested | 6 major, 1 minor | 412,907 | 23m 41s |
| 2 | task-report-layout-2 | changes_requested | gate failed | 98,402 | 6m 05s |
| 3 | task-report-layout-3 | changes_requested | 1 major; 1 reviewer failed | 233,018 | 15m 12s |
| 4 | task-report-layout-4 | pass | none | 201,555 | 12m 30s |
| 5 | verify-task-report-layout | verified | — | 41,230 | 4m 02s |
|  | Total: 22 Attempts (2 failed) |  |  | 987,112 | 1h 01m |

<details>
<summary>Round 1 · task-report-layout: 2 runs, 1h 02m wall, 1 failed Attempt (1 provider_failure; 3,120 tokens)</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.implement | implement | codex gpt-6-sol/high | 6 (1 failed) | 412,907 | 23m 00s | - |

</details>

<details>
<summary>Round 2 · task-report-layout-2: 1 run, 6m 40s wall, no failed Attempt</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.review.nodes.checks | check | - | 3 | 98,402 | 6m 05s | fmt passed 2.1s, clippy failed 2m 10s |

</details>

<details>
<summary>Round 3 · task-report-layout-3: 1 run, 16m 40s wall, 1 failed Attempt (1 process_failure; 0 tokens)</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.review.nodes.bugs | review | codex gpt-6-sol/high | 6 (1 failed) | 233,018 | 15m 00s | - |

</details>

<details>
<summary>Round 4 · task-report-layout-4: 1 run, 13m 20s wall, no failed Attempt</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.implement | implement | codex gpt-6-sol/high | 5 | 201,555 | 11m 40s | - |

</details>

<details>
<summary>Round 5 · verify-task-report-layout: 1 run, 4m 10s wall, no failed Attempt</summary>

| Node | Role | Worker | Attempts | Tokens | Elapsed | Checks |
| --- | --- | --- | ---: | ---: | ---: | --- |
| root.nodes.evaluate | evaluate | claude claude-opus-5-5/high | 2 | 41,230 | 4m 00s | - |

</details>
<!-- /af-task-report -->
";
    assert_eq!(markdown(&report), expected);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["pipelines"][0]["steps"][2]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(json["tasks"][4]["round"], 5);
    assert!(json["tasks"][4].get("findings").is_none());
    assert_eq!(
        json["tasks"][1]["findings"],
        serde_json::json!({"blocker": 0, "major": 0, "minor": 0, "review_ran": false,
            "gate_failed": true, "failed_reviewers": 0})
    );
}

#[test]
fn a_findings_cell_says_what_the_review_recorded() {
    let cell = |findings: Option<TaskReportFindingsV1>, collected: bool| {
        let mut task = entry("a", "gpt-6-sol");
        task.findings = findings;
        task.collected = collected;
        findings_cell(&task)
    };
    let ran = |blocker, major, minor, failed_reviewers| {
        Some(TaskReportFindingsV1 {
            blocker: Some(blocker),
            major: Some(major),
            minor: Some(minor),
            review_ran: true,
            gate_failed: false,
            failed_reviewers,
        })
    };
    assert_eq!(cell(ran(0, 6, 1, 0), false), "6 major, 1 minor");
    assert_eq!(cell(ran(2, 0, 3, 0), false), "2 blocker, 3 minor");
    assert_eq!(cell(ran(0, 0, 0, 0), false), "none");
    assert_eq!(cell(ran(0, 0, 0, 2), false), "none; 2 reviewers failed");
    assert_eq!(cell(ran(1, 0, 0, 1), false), "1 blocker; 1 reviewer failed");
    let not_ran = |gate_failed| {
        Some(TaskReportFindingsV1 {
            blocker: Some(0),
            major: Some(0),
            minor: Some(0),
            review_ran: false,
            gate_failed,
            failed_reviewers: 0,
        })
    };
    assert_eq!(cell(not_ran(true), false), "gate failed");
    assert_eq!(cell(not_ran(false), false), "not run");
    assert_eq!(cell(None, false), "—");
    assert_eq!(cell(None, true), "unknown");
    // The review ran but a round has no complete finding set: unknown, never `none`.
    let incomplete = |failed_reviewers| {
        Some(TaskReportFindingsV1 {
            blocker: None,
            major: None,
            minor: None,
            review_ran: true,
            gate_failed: false,
            failed_reviewers,
        })
    };
    assert_eq!(cell(incomplete(0), false), "unknown");
    assert_eq!(cell(incomplete(1), false), "unknown; 1 reviewer failed");
}

/// A round whose gather was incomplete: the `bugs` reviewer's result was admitted, its
/// sibling `correctness` is missing, so the reduce step wrote no finding set.
fn incomplete_round() -> TaskReviewRoundV1 {
    use review_core::task::pipeline::ReceiptOutcomeV1;
    use review_core::task::review::ReviewConclusionV1;
    let digest = |n: u8| format!("sha256:{}", format!("{n:02x}").repeat(32));
    let round = TaskReviewRoundV1 {
        invocation: review_core::task::execution::TaskInvocationV1 {
            plan_id: digest(1),
            node: "root.nodes.reduce".into(),
            inputs: BTreeMap::new(),
        },
        policy_id: digest(2),
        subject_id: digest(3),
        snapshot_id: digest(4),
        round: 1,
        outcome: ReceiptOutcomeV1::Inconclusive,
        conclusion: ReviewConclusionV1::Incomplete,
        selected_results: BTreeMap::from([("bugs".to_owned(), digest(5))]),
        missing_reviewers: BTreeSet::from(["correctness".to_owned()]),
        finding_set_id: None,
        demand_set_id: None,
    };
    round.validate().unwrap();
    round
}

/// A review that ran without a complete finding set for every recorded round has unknown
/// counts, in the document and in the cell: an incomplete round never reads as `none`.
#[test]
fn an_incomplete_round_reports_unknown_findings_never_none() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let reviewed = ReviewAttempts {
        reviewers: true,
        failed_reviewers: 1,
        check_failed: false,
    };
    let unknown = TaskReportFindingsV1 {
        blocker: None,
        major: None,
        minor: None,
        review_ran: true,
        gate_failed: false,
        failed_reviewers: 1,
    };
    // The admitted reviewer result alone says the review ran.
    for review in [reviewed.clone(), ReviewAttempts::default()] {
        let failed_reviewers = review.failed_reviewers;
        assert_eq!(
            super::findings(&cas, &[incomplete_round()], &review).unwrap(),
            TaskReportFindingsV1 {
                failed_reviewers,
                ..unknown.clone()
            }
        );
    }
    // Reviewers began but no round was recorded yet: unknown too.
    assert_eq!(super::findings(&cas, &[], &reviewed).unwrap(), unknown);
    // Before any review ran, nothing was found.
    assert_eq!(
        super::findings(&cas, &[], &ReviewAttempts::default()).unwrap(),
        TaskReportFindingsV1 {
            blocker: Some(0),
            major: Some(0),
            minor: Some(0),
            review_ran: false,
            gate_failed: false,
            failed_reviewers: 0,
        }
    );

    let mut task = entry("incomplete", "gpt-6-sol");
    task.review_rounds = Some(1);
    task.findings = Some(super::findings(&cas, &[incomplete_round()], &reviewed).unwrap());
    let report = report(vec![task]);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["tasks"][0]["findings"],
        serde_json::json!({"review_ran": true, "gate_failed": false, "failed_reviewers": 1}),
        "the counts are absent, unknown"
    );
    let text = markdown(&report);
    assert!(
        text.contains("| 1 | incomplete | pass | unknown; 1 reviewer failed | 1,234 | 3.2s |"),
        "{text}"
    );
}

#[test]
fn a_stage_of_several_steps_names_each_steps_nodes() {
    let pipeline = TaskReportPipelineV1 {
        name: "fixture/split".into(),
        version: "2".into(),
        steps: vec![
            step(
                1,
                "review",
                &["root.nodes.bugs"],
                model("codex", "gpt-6-sol"),
                &[],
            ),
            step(
                1,
                "review",
                &["root.nodes.correctness"],
                model("claude", "claude-opus-5-5"),
                &[],
            ),
            step(2, "gate", &["root.nodes.a", "root.nodes.b"], None, &["fmt"]),
        ],
    };
    assert_eq!(
        pipeline_line(&pipeline),
        "**fixture/split@2**: review (bugs: codex gpt-6-sol/high) + review (correctness: \
         claude claude-opus-5-5/high) → gate (a, b: fmt)"
    );
    let empty = TaskReportPipelineV1 {
        steps: Vec::new(),
        ..pipeline
    };
    assert_eq!(
        pipeline_line(&empty),
        "**fixture/split@2**: no Worker or check step"
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
    let text = markdown(&report(vec![task]));
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
    let text = markdown(&report(vec![task]));
    let row = text
        .lines()
        .find(|line| line.starts_with("| 1 | a |"))
        .unwrap();
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
    let report = report(vec![task]);
    let text = markdown(&report);
    assert!(text.contains("| — | 205,295 | 3.2s |"), "{text}");
    assert!(
        text.contains("| Total: 2 Attempts (1 failed) |  |  | 205,295 | 3.2s |"),
        "{text}"
    );
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

/// Every value of the shared table is copied unchanged into both forms, in the pipeline line
/// and in the node breakdown, when it is a model identity; any other value leaves both forms
/// exactly as an `unknown` model does.
#[test]
fn only_a_model_identity_is_copied_into_the_report() {
    let with =
        |model: &str| TaskReportV1::new(vec![plain(model)], vec![entry("a", model)]).unwrap();
    let unknown = with(TASK_REPORT_UNKNOWN_MODEL);
    for (recorded, identity) in TASK_REPORT_MODEL_CASES {
        let shown = report_model(recorded);
        let report = with(&shown);
        let text = markdown(&report);
        let json = serde_json::to_value(&report).unwrap();
        if identity {
            assert_eq!(shown, recorded);
            assert_eq!(json["tasks"][0]["nodes"][0]["worker"]["model"], recorded);
            assert_eq!(
                json["pipelines"][0]["steps"][0]["worker"]["model"],
                recorded
            );
            assert!(
                text.contains(&format!("| codex {recorded}/high |")),
                "{text}"
            );
            assert!(
                text.contains(&format!("implement (codex {recorded}/high) →")),
                "{text}"
            );
        } else {
            assert_eq!(shown, TASK_REPORT_UNKNOWN_MODEL, "{recorded:?}");
            assert_eq!(text, markdown(&unknown), "{recorded:?}");
            assert_eq!(
                json,
                serde_json::to_value(&unknown).unwrap(),
                "{recorded:?}"
            );
        }
    }
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

/// Issue #191: only a started Attempt makes a lease a run. `af task start --execute` (epoch 1)
/// starts and settles every Attempt, and its writer dies before publishing; `af task run`
/// (epoch 2) resumes after all Attempts finished, publishes the selected results and finishes
/// the Task. The resume began no Attempt, so it is not a run and its span is not active time.
#[test]
fn a_resume_after_every_attempt_finished_is_not_a_run() {
    use review_core::task::execution::TaskExecutionRecordV1 as Record;
    let id = || "attempt-1".to_owned();
    let digest = || format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        record_mark(Some(&Record::Started { attempt_id: id() })),
        Mark::Work
    );
    for record in [
        Record::Invocation {
            invocation_id: digest(),
        },
        Record::Settled {
            attempt_id: id(),
            charged_tokens: 10,
            result: TaskAttemptResultV1::Succeeded {
                output_id: digest(),
            },
            raw_artifact_ids: Vec::new(),
            usage_id: None,
            unknown_usage: None,
        },
        Record::Published {
            output_id: digest(),
            attempt_id: Some(id()),
        },
        Record::Released {
            attempt_id: id(),
            reason: "recovered".into(),
        },
        Record::Settled {
            attempt_id: id(),
            charged_tokens: 0,
            result: TaskAttemptResultV1::Abandoned {
                diagnostic_id: digest(),
            },
            raw_artifact_ids: Vec::new(),
            usage_id: None,
            unknown_usage: None,
        },
    ] {
        assert_eq!(record_mark(Some(&record)), Mark::Other, "{record:?}");
    }
    // A collected Task's records are gone: each recorded execution still counts.
    assert_eq!(record_mark(None), Mark::Work);

    let started = record_mark(Some(&Record::Started { attempt_id: id() }));
    let settled = record_mark(Some(&Record::Settled {
        attempt_id: id(),
        charged_tokens: 10,
        result: TaskAttemptResultV1::Succeeded {
            output_id: digest(),
        },
        raw_artifact_ids: Vec::new(),
        usage_id: None,
        unknown_usage: None,
    }));
    let published = record_mark(Some(&Record::Published {
        output_id: digest(),
        attempt_id: Some(id()),
    }));
    let events = [
        (1, 1_000, Mark::Other),
        (1, 1_010, started),
        (1, 1_600, settled),
        (2, 50_000, Mark::Other),
        (2, 50_020, published),
        (2, 50_900, Mark::Other),
    ];
    assert_eq!(
        tally_runs(&events),
        Some(EventTimes {
            runs: 1,
            wall_ms: 49_900,
            active_ms: 600,
        })
    );
}

/// One node ran an Attempt under the plan before `af task refresh` and two under the plan
/// after it, which binds its slot to another Worker: each Worker gets a row with only its own
/// Attempts, failures and tokens.
#[test]
fn a_node_refreshed_onto_another_worker_charges_each_worker_its_own_attempts() {
    let attempt = |node: &str, worker, failure, tokens| CountedAttempt {
        node: node.into(),
        role: "implement".into(),
        worker,
        reviewer: false,
        failure,
        tokens,
        elapsed_ms: Some(1_000),
        checks: Vec::new(),
        unknown_usage: None,
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
    let text = markdown(&report(vec![task]));
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

/// ADR-0143: Attempts whose usage is unknown are tallied by cause and on their node, and every
/// Tokens cell they touch reads the charged total followed by `(+N unknown)`; the round's
/// details name the causes. A Task whose usage is all known renders exactly as before.
#[test]
fn unknown_usage_reads_as_unknown_beside_the_charged_total_never_as_spend() {
    use review_core::task::usage::TaskUnknownUsageCauseV1 as Cause;
    let attempt = |worker, failure, tokens, unknown_usage| CountedAttempt {
        unknown_usage,
        node: "root.nodes.implement".into(),
        role: "implement".into(),
        worker,
        reviewer: false,
        failure,
        tokens,
        elapsed_ms: Some(1_000),
        checks: Vec::new(),
    };
    let failed = Some(TaskReportFailureClassV1::ProviderFailure);
    let (summary, nodes) = tally_attempts(vec![
        attempt(
            model("codex", "gpt-6-sol"),
            failed,
            0,
            Some(Cause::Capacity),
        ),
        attempt(
            model("codex", "gpt-6-sol"),
            Some(TaskReportFailureClassV1::Abandoned),
            0,
            Some(Cause::LeaseExpired),
        ),
        attempt(
            model("codex", "gpt-6-sol"),
            failed,
            0,
            Some(Cause::Capacity),
        ),
        attempt(model("codex", "gpt-6-sol"), None, 205_295, None),
    ])
    .unwrap();
    assert_eq!(summary.unknown_usage, 3);
    assert_eq!(
        summary.unknown_usage_causes,
        [
            TaskReportUnknownUsageV1 {
                cause: Cause::Capacity,
                attempts: 2,
            },
            TaskReportUnknownUsageV1 {
                cause: Cause::LeaseExpired,
                attempts: 1,
            },
        ]
    );
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].unknown_usage, 3);
    assert_eq!(nodes[0].tokens.get(), 205_295);
    let mut task = entry("a", "gpt-6-sol");
    task.attempts = Some(TaskReportAttemptsV1 {
        total: 4,
        ..summary
    });
    task.chargeable_tokens = DecimalU128::from(205_295);
    task.nodes = Some(nodes);
    let text = markdown(&report(vec![task.clone()]));
    assert!(
        text.contains("| 1 | a | pass | — | 205,295 (+3 unknown) | 3.2s |"),
        "{text}"
    );
    assert!(
        text.contains("|  | Total: 4 Attempts (3 failed) |  |  | 205,295 (+3 unknown) | 3.2s |"),
        "{text}"
    );
    assert!(
        text.contains("| 4 (3 failed) | 205,295 (+3 unknown) |"),
        "{text}"
    );
    assert!(
        text.contains(
            "; 0 tokens), 3 Attempts' usage unknown (2 capacity, 1 lease expired)</summary>"
        ),
        "{text}"
    );
    // One cause is named alone, and one Attempt reads in the singular.
    let mut single = task;
    let attempts = single.attempts.as_mut().unwrap();
    attempts.unknown_usage = 1;
    attempts.unknown_usage_causes = vec![TaskReportUnknownUsageV1 {
        cause: Cause::Capacity,
        attempts: 1,
    }];
    single.nodes.as_mut().unwrap()[0].unknown_usage = 1;
    let text = markdown(&report(vec![single]));
    assert!(
        text.contains(", 1 Attempt's usage unknown (capacity)</summary>"),
        "{text}"
    );
    assert!(text.contains(" | 205,295 (+1 unknown) | "), "{text}");
    // All usage known: no suffix and no cause.
    let known = markdown(&report(vec![entry("b", "gpt-6-sol")]));
    assert!(!known.contains("unknown"), "{known}");
}

/// A reviewer whose Attempts failed counts once however often it failed; a check that failed
/// is a failed gate only while no reviewer began.
#[test]
fn reviewer_attempts_say_whether_the_review_ran_and_how_many_reviewers_failed() {
    let attempt = |node: &str, reviewer, failed: bool, check: Option<TaskReportCheckStatusV1>| {
        CountedAttempt {
            node: node.into(),
            role: if reviewer { "review" } else { "check" }.into(),
            worker: None,
            reviewer,
            failure: failed.then_some(TaskReportFailureClassV1::ProcessFailure),
            tokens: 0,
            elapsed_ms: None,
            checks: check
                .map(|status| TaskReportCheckV1 {
                    name: "test".into(),
                    status,
                    elapsed_ms: None,
                })
                .into_iter()
                .collect(),
            unknown_usage: None,
        }
    };
    let failed_gate = [attempt(
        "root.nodes.checks",
        false,
        false,
        Some(TaskReportCheckStatusV1::Failed),
    )];
    assert_eq!(
        ReviewAttempts::of(&failed_gate),
        ReviewAttempts {
            reviewers: false,
            failed_reviewers: 0,
            check_failed: true,
        }
    );
    let reviewed = [
        attempt(
            "root.nodes.checks",
            false,
            false,
            Some(TaskReportCheckStatusV1::Passed),
        ),
        attempt("root.nodes.bugs", true, true, None),
        attempt("root.nodes.bugs", true, true, None),
        attempt("root.nodes.correctness", true, false, None),
    ];
    assert_eq!(
        ReviewAttempts::of(&reviewed),
        ReviewAttempts {
            reviewers: true,
            failed_reviewers: 1,
            check_failed: false,
        }
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
    let text = markdown(&report(vec![collected, untimed]));
    assert!(
        text.contains("| 1 | old | pass (collected) | unknown | 1,234 | 3.2s |"),
        "{text}"
    );
    assert!(
        text.contains("|  | Total: unknown Attempts |  |  | 2,468 | 6.5s |"),
        "{text}"
    );
    assert!(
        text.contains("<summary>Round 1 · old: 2 runs, 2h 00m wall</summary>"),
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

/// A Task that names no pipeline, collected or never planned, still leads the block with a
/// pipeline line: `**unknown pipeline**: not retained`, once, after the known pipelines.
#[test]
fn a_task_without_a_retained_plan_gets_the_unknown_pipeline_line_once() {
    let collected = |id: &str| {
        let mut task = entry(id, "gpt-6-sol");
        task.collected = true;
        task.attempts = None;
        task.nodes = None;
        task.review_rounds = None;
        task.pipeline = None;
        task
    };
    let only = markdown(
        &TaskReportV1::new(Vec::new(), vec![collected("old"), collected("older")]).unwrap(),
    );
    assert_eq!(
        only.lines()
            .filter(|line| line.starts_with("**"))
            .collect::<Vec<_>>(),
        [TASK_REPORT_UNKNOWN_PIPELINE],
        "{only}"
    );
    assert!(
        only.contains(&format!(
            "### af task report\n\n{TASK_REPORT_UNKNOWN_PIPELINE}\n\n| Round |"
        )),
        "{only}"
    );
    let mixed = markdown(&report(vec![collected("old"), entry("new", "gpt-6-sol")]));
    assert_eq!(
        mixed
            .lines()
            .filter(|line| line.starts_with("**"))
            .collect::<Vec<_>>(),
        [
            "**fixture/plain@1.0.0**: implement (codex gpt-6-sol/high) → gate (pagination)",
            TASK_REPORT_UNKNOWN_PIPELINE
        ],
        "{mixed}"
    );
    // Every Task names its pipeline: no unknown line.
    let known = markdown(&report(vec![entry("new", "gpt-6-sol")]));
    assert!(!known.contains(TASK_REPORT_UNKNOWN_PIPELINE), "{known}");
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
