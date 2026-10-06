//! `af task report` end to end (ADR-0142), over Stores the existing fixtures build: a Task
//! interrupted and resumed once, a document Task whose model Worker fails at its Provider, and
//! two Tasks of one Store. Every `--json` document validates against
//! `schemas/task-report-v1.json`, every Markdown block passes `scripts/check-pr-report.py`, and
//! neither carries a Provider label, a path or an account the Store or the machine holds.

use nix::sys::signal::Signal;
use review_store::{Cas, EventStore};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::schemas;
use crate::task_document::provider_admission::Fixture;
use crate::{task_cli, task_gc, task_interrupt};

const AF: &str = env!("CARGO_BIN_EXE_af");

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn report(repo: &Path, state: &Path, ids: &[&str], json: bool) -> Output {
    let mut command = Command::new(AF);
    command
        .current_dir(repo)
        .args(["task", "report"])
        .args(ids)
        .arg("--state")
        .arg(state);
    if json {
        command.arg("--json");
    }
    command.output().unwrap()
}

fn stdout(output: &Output) -> String {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// The `--json` document, validated against the published schema.
fn document(repo: &Path, state: &Path, ids: &[&str]) -> Value {
    let value: Value = serde_json::from_str(&stdout(&report(repo, state, ids, true))).unwrap();
    schemas::valid(&schemas::validator("task-report-v1.json"), &value);
    value
}

/// `scripts/check-pr-report.py --body` over `body`: its exit status and what it printed.
fn check_pr_report(body: &str) -> (bool, String) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("body.md");
    std::fs::write(&path, body).unwrap();
    let output = Command::new("python3")
        .arg("-I")
        .arg(workspace().join("scripts/check-pr-report.py"))
        .arg("--body")
        .arg(&path)
        .output()
        .unwrap();
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

/// A pull request description around `block`, as the template lays it out.
fn description(block: &str) -> String {
    format!(
        "## What\n\nPaginate.\n\n## af task report\n\n{block}\n## Checklist\n\n- [x] `make check` passes locally.\n"
    )
}

/// The rendered block passes the pull request check, and removing any required part fails it.
fn assert_block_is_checked(markdown: &str) {
    let (passed, said) = check_pr_report(&description(markdown));
    assert!(passed, "{said}\n{markdown}");
    let header = markdown
        .lines()
        .find(|line| line.starts_with("| Task |"))
        .unwrap();
    let rows: Vec<&str> = markdown
        .lines()
        .skip_while(|line| !line.starts_with("| --- |"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect();
    assert!(!rows.is_empty(), "{markdown}");
    let totals = markdown
        .lines()
        .find(|line| line.starts_with("**Totals:**"))
        .unwrap();
    let mut without_rows = markdown.to_owned();
    for row in &rows {
        without_rows = without_rows.replace(&format!("{row}\n"), "");
    }
    for (part, body) in [
        (
            "begin marker",
            markdown.replace("<!-- af-task-report:v1 -->\n", ""),
        ),
        (
            "end marker",
            markdown.replace("<!-- /af-task-report -->\n", ""),
        ),
        (
            "a column",
            markdown.replace(header, &header.replace(" Wall time |", "")),
        ),
        ("the Task rows", without_rows),
        (
            "the totals line",
            markdown.replace(&format!("{totals}\n"), ""),
        ),
        ("the whole block", String::new()),
        ("nothing: a second block", format!("{markdown}{markdown}")),
    ] {
        let (passed, said) = check_pr_report(&description(&body));
        assert!(!passed, "the check passed without {part}:\n{body}");
        assert!(said.contains("af task report TASK_ID"), "{said}");
    }
}

/// Every file of the Store but SQLite's shared-memory index, which any reader of a WAL database
/// maps and marks, this test's own included.
fn store_files(state: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = task_gc::tree(state);
    files.retain(|(path, _)| !path.to_string_lossy().ends_with("-shm"));
    files
}

/// Nothing machine-local leaves the Store through the report: no Provider registry label, no
/// path under the test root (state, home, auth directory, repository), no account.
fn assert_private(texts: &[&str], forbidden: &[&str]) {
    for text in texts {
        for needle in forbidden {
            assert!(!text.contains(needle), "`{needle}` leaked into:\n{text}");
        }
    }
}

#[test]
fn a_task_resumed_once_reports_two_runs_and_active_time_below_its_wall_time() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let ready = directory.path().join("worker-ready");
    let resume = directory.path().join("worker-resume");
    task_interrupt::long_running_implementer(&repo, &ready, &resume);
    let out = task_interrupt::interrupt_running_worker(&repo, &state, &ready, &[Signal::SIGINT]);
    assert_ne!(out.status.code(), Some(0));
    // Time between the two runs is waiting, not work.
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    std::fs::write(&resume, b"").unwrap();
    let resumed = Command::new(AF)
        .current_dir(&repo)
        .args(["task", "run", task_interrupt::TASK, "--state"])
        .arg(&state)
        .output()
        .unwrap();
    stdout(&resumed);

    let value = document(&repo, &state, &[task_interrupt::TASK]);
    let task = &value["tasks"][0];
    assert_eq!(task["task_id"], task_interrupt::TASK);
    assert_eq!(task["kind"], "implement");
    assert_eq!(task["runs"], 2, "{task:#}");
    let (active, wall) = (
        task["active_ms"].as_u64().unwrap(),
        task["wall_ms"].as_u64().unwrap(),
    );
    assert!(active < wall, "{task:#}");
    assert!(
        wall - active >= 1_500,
        "the wait between runs is not work: {task:#}"
    );

    // The outcome, Attempts and tokens are the ones `af task show` states.
    let shown: Value = serde_json::from_str(&stdout(
        &Command::new(AF)
            .current_dir(&repo)
            .args(["task", "show", task_interrupt::TASK, "--json", "--state"])
            .arg(&state)
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(task["outcome"], shown["result"]["domain_conclusion"]);
    assert_eq!(task["attempts"]["total"], shown["attempts"]);
    assert_eq!(task["chargeable_tokens"], shown["chargeable_tokens"]);
    let text = String::from_utf8(
        Command::new(AF)
            .current_dir(&repo)
            .args(["task", "show", task_interrupt::TASK, "--state"])
            .arg(&state)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        text.starts_with(&format!(
            "Task {}: {}\n",
            task_interrupt::TASK,
            task["outcome"].as_str().unwrap()
        )),
        "{text}"
    );

    // The interrupted Attempt is the one failure, of the implementer, with its recorded charge.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let projection = store
        .task_projection(&cas, task_interrupt::TASK)
        .unwrap()
        .unwrap();
    let accounting = projection.execution.unwrap().attempt_accounting();
    let failed: Vec<_> = accounting
        .iter()
        .filter(|a| {
            matches!(
                a.result,
                Some(review_core::task::execution::TaskAttemptResultV1::Failed { .. })
            )
        })
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(task["attempts"]["failed"], 1, "{task:#}");
    assert_eq!(
        task["attempts"]["failed_tokens"],
        failed[0].charged_tokens.to_string()
    );
    assert_eq!(task["attempts"]["failures"].as_array().unwrap().len(), 1);
    let nodes = task["nodes"].as_array().unwrap();
    let implement = nodes
        .iter()
        .find(|node| node["node"] == failed[0].reservation.node)
        .unwrap();
    assert_eq!(implement["role"], "implement");
    assert_eq!(implement["worker"]["kind"], "command");
    assert_eq!(implement["attempts"], 2);
    assert_eq!(implement["failed_attempts"], 1);
    assert_eq!(
        nodes
            .iter()
            .map(|node| node["attempts"].as_u64().unwrap())
            .sum::<u64>(),
        task["attempts"]["total"].as_u64().unwrap()
    );
    let check = nodes
        .iter()
        .flat_map(|node| node["checks"].as_array().unwrap())
        .find(|check| check["name"] == "pagination")
        .unwrap_or_else(|| panic!("the gate's check is reported: {task:#}"));
    assert_eq!(check["status"], "passed");
    assert!(check["elapsed_ms"].is_u64(), "{check}");

    drop(store);
    let before = store_files(&state);
    let markdown = stdout(&report(&repo, &state, &[task_interrupt::TASK], false));
    assert!(
        markdown.contains(&format!(
            "<summary>{}: 2 runs, 1 failed Attempt (1 {}; ",
            task_interrupt::TASK,
            task["attempts"]["failures"][0]["class"].as_str().unwrap()
        )),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
    // `AF_TASK_REPORT_FIXTURE=DIR` writes this Store's report as `fixtures/task-report` holds
    // it: the pretty JSON document and the Markdown the renderer prints for it.
    if let Some(fixture) = std::env::var_os("AF_TASK_REPORT_FIXTURE") {
        let fixture = PathBuf::from(fixture);
        let mut pretty = serde_json::to_string_pretty(&value).unwrap();
        pretty.push('\n');
        std::fs::write(fixture.join("report.json"), pretty).unwrap();
        std::fs::write(fixture.join("report.md"), &markdown).unwrap();
    }
    assert_private(
        &[&markdown, &value.to_string()],
        &[
            directory.path().to_str().unwrap(),
            state.to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    );
    assert_eq!(document(&repo, &state, &[task_interrupt::TASK]), value);
    assert_eq!(store_files(&state), before, "the report wrote nothing");
}

#[test]
fn a_provider_failure_is_counted_with_its_class_and_charge_and_the_provider_stays_private() {
    let f = Fixture::new(true, 5712, 49152);
    // The fixture's native Codex answers the capability probe and then fails the author's call.
    let codex = f._root.path().join("bin/codex");
    let native = std::fs::read_to_string(&codex).unwrap();
    let author = " kind='author'; r=json.loads(request)\n";
    assert_eq!(native.matches(author).count(), 1);
    std::fs::write(
        &codex,
        native.replace(
            author,
            " sys.stderr.write('synthetic provider outage\\n'); sys.exit(1)\n",
        ),
    )
    .unwrap();
    let planned = f.cli(&["task", "plan", "--file", "document.json"]);
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let ran = f.cli(&["task", "run", "--execute", "release-notes"]);
    let done: Value = serde_json::from_slice(&ran.stdout).unwrap_or_else(|_| {
        panic!(
            "{}\n{}",
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr)
        )
    });

    let run = |json: bool| {
        let mut command = Command::new(AF);
        command
            .current_dir(&f.repo)
            .env("HOME", &f.home)
            .env("PATH", &f.path)
            .env("AF_PROVIDERS_FILE", f.home.join("providers.toml"))
            .args(["task", "report", "release-notes", "--state"])
            .arg(&f.state);
        if json {
            command.arg("--json");
        }
        stdout(&command.output().unwrap())
    };
    let value: Value = serde_json::from_str(&run(true)).unwrap();
    schemas::valid(&schemas::validator("task-report-v1.json"), &value);
    let task = &value["tasks"][0];
    assert_eq!(task["kind"], "release-note");
    assert_eq!(task["runs"], 1);
    assert_eq!(task["attempts"]["total"], done["attempts"]);
    assert_eq!(task["chargeable_tokens"], done["chargeable_tokens"]);
    let outcome = done["result"]["domain_conclusion"]
        .as_str()
        .unwrap_or("planned");
    assert_eq!(task["outcome"], outcome);

    let cas = Cas::open_existing(f.state.join("cas")).unwrap();
    let store = EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
    let accounting = store
        .task_projection(&cas, "release-notes")
        .unwrap()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting();
    let author = accounting
        .iter()
        .find(|a| a.reservation.node == "root.nodes.author" && a.started)
        .expect("the author's Attempt began");
    assert!(matches!(
        author.result,
        Some(review_core::task::execution::TaskAttemptResultV1::Failed { .. })
    ));
    assert_eq!(task["attempts"]["failed"], 1, "{task:#}");
    assert_eq!(
        task["attempts"]["failures"][0],
        serde_json::json!({
            "class": "provider_failure",
            "attempts": 1,
            "tokens": author.charged_tokens.to_string(),
        })
    );
    assert_eq!(
        task["attempts"]["failed_tokens"],
        author.charged_tokens.to_string()
    );
    let node = task["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node"] == "root.nodes.author")
        .unwrap();
    assert_eq!(
        node["worker"],
        serde_json::json!({"kind":"model","provider_kind":"codex","model":"codex-fixture-1","effort":"high"})
    );
    let admission = task["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node"] == "root.providers.admit0")
        .unwrap();
    assert_eq!(admission["role"], "provider_admission");
    assert_eq!(admission["tokens"], "5712");

    let markdown = run(false);
    assert!(
        markdown.contains("| codex codex-fixture-1/high |"),
        "{markdown}"
    );
    assert!(
        markdown.contains("1 failed Attempt (1 provider_failure; "),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
    assert_private(
        &[&markdown, &value.to_string()],
        &[
            "codex-personal",
            "fixture@example.invalid",
            "synthetic provider outage",
            f._root.path().to_str().unwrap(),
            f.home.to_str().unwrap(),
            f.state.to_str().unwrap(),
        ],
    );
}

#[test]
fn several_tasks_give_one_row_each_in_order_and_an_unknown_id_prints_nothing() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_gc::fixture(root.path());
    task_gc::start(&repo, &state, "gc-older.json");
    task_gc::start(&repo, &state, "gc-newer.json");

    let value = document(&repo, &state, &["gc-newer", "gc-older"]);
    let tasks = value["tasks"].as_array().unwrap();
    let ids: Vec<&str> = tasks
        .iter()
        .map(|t| t["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gc-newer", "gc-older"], "in the order given");
    let sum = |field: &str| -> u64 { tasks.iter().map(|t| t[field].as_u64().unwrap()).sum() };
    let totals = &value["totals"];
    assert_eq!(totals["tasks"], 2);
    assert_eq!(totals["active_ms"], sum("active_ms"));
    assert_eq!(
        totals["attempts"],
        tasks
            .iter()
            .map(|t| t["attempts"]["total"].as_u64().unwrap())
            .sum::<u64>()
    );
    assert_eq!(totals["failed_attempts"], 0);
    assert_eq!(totals["review_rounds"], 0);
    assert_eq!(
        totals["chargeable_tokens"],
        tasks
            .iter()
            .map(|t| t["chargeable_tokens"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap())
            .sum::<u128>()
            .to_string()
    );
    for task in tasks {
        assert_eq!(task["pipeline"], "fixture/plain@1.0.0");
        assert_eq!(task["runs"], 1);
        assert_eq!(task["attempts"]["failed"], 0);
    }

    let markdown = stdout(&report(&repo, &state, &["gc-newer", "gc-older"], false));
    let rows: Vec<&str> = markdown
        .lines()
        .filter(|line| line.starts_with("| gc-"))
        .collect();
    assert_eq!(rows.len(), 2, "{markdown}");
    assert!(rows[0].starts_with("| gc-newer | implement | fixture/plain@1.0.0 |"));
    assert!(rows[1].starts_with("| gc-older | implement | fixture/plain@1.0.0 |"));
    assert!(
        markdown.contains("**Totals:** 2 Tasks · 0 rounds · "),
        "{markdown}"
    );
    assert_eq!(markdown.matches("<details>").count(), 2);
    assert_block_is_checked(&markdown);

    for ids in [
        ["gc-older", "missing-task"].as_slice(),
        ["missing-task"].as_slice(),
    ] {
        let refused = report(&repo, &state, ids, false);
        assert_eq!(refused.status.code(), Some(1));
        assert!(
            refused.stdout.is_empty(),
            "nothing but the error is printed"
        );
        let said = String::from_utf8_lossy(&refused.stderr);
        assert!(said.contains("Task `missing-task` was not found"), "{said}");
    }
    let twice = report(&repo, &state, &["gc-older", "gc-older"], false);
    assert_eq!(twice.status.code(), Some(1));
    assert!(twice.stdout.is_empty());
    let empty = tempfile::tempdir().unwrap();
    let none = report(&repo, empty.path(), &["gc-older"], false);
    assert_eq!(none.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&none.stderr).contains("Task `gc-older` was not found"));

    // A collected Task keeps its row from the tombstone: its Attempts are unknown, not zero.
    let collected = Command::new(AF)
        .current_dir(&repo)
        .args([
            "task",
            "gc",
            "--older-than",
            "0",
            "--keep",
            "1",
            "--apply",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    stdout(&collected);
    let value = document(&repo, &state, &["gc-older", "gc-newer"]);
    let older = &value["tasks"][0];
    assert_eq!(older["collected"], true);
    assert_eq!(older["runs"], 1);
    assert!(older.get("attempts").is_none() && older.get("nodes").is_none());
    assert!(value["totals"].get("attempts").is_none(), "{value:#}");
    let markdown = stdout(&report(&repo, &state, &["gc-older", "gc-newer"], false));
    assert!(
        markdown.contains("(collected) | unknown | unknown |"),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
}
