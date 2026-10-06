//! `af task report` end to end (ADR-0142), over Stores the existing fixtures build: a Task
//! interrupted and resumed once, a document Task whose model Worker fails at its Provider (once
//! with no usage report at capacity, ADR-0143), two
//! Tasks of one Store (one of them later collected, and reported alone), and two Tasks under two
//! pipelines, one whose review round recorded a major and a minor finding. Every `--json` document validates against
//! `schemas/task-report-v1.json`, every Markdown block passes `scripts/check-pr-report.py`, and
//! neither carries a Provider label, a path or an account the Store or the machine holds.

use nix::sys::signal::Signal;
use review_core::task::task_report::{
    TASK_REPORT_BEGIN, TASK_REPORT_UNKNOWN_PIPELINE, is_model_identity,
};
use review_store::{Cas, EventStore};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::schemas;
use crate::task_document::provider_admission::Fixture;
use crate::{task_cli, task_document, task_gc, task_interrupt};

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
        .find(|line| line.starts_with("| Round |"))
        .unwrap();
    // The round rows, then the totals row.
    let mut rows: Vec<&str> = markdown
        .lines()
        .skip_while(|line| !line.starts_with("| ---: |"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .collect();
    let total = rows.pop().unwrap();
    assert!(total.starts_with("|  | Total: "), "{markdown}");
    assert!(!rows.is_empty(), "{markdown}");
    let pipelines: Vec<&str> = markdown
        .lines()
        .filter(|line| line.starts_with("**") && line.contains("**: "))
        .collect();
    assert!(!pipelines.is_empty(), "{markdown}");
    let mut without_rows = markdown.to_owned();
    for row in &rows {
        without_rows = without_rows.replace(&format!("{row}\n"), "");
    }
    let mut without_pipelines = markdown.to_owned();
    for line in &pipelines {
        without_pipelines = without_pipelines.replace(&format!("{line}\n"), "");
    }
    let placeholder = markdown.replace(&format!("{}\n", rows[0]), "| x |\n");
    let mut cells: Vec<&str> = rows[0].split(" | ").collect();
    cells[1] = "<!--x-->";
    let hidden = markdown.replace(
        &format!("{}\n", rows[0]),
        &format!("{}\n", cells.join(" | ")),
    );
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
            markdown.replace(header, &header.replace(" Findings |", "")),
        ),
        (
            "nothing: an extra column",
            markdown.replace(header, &format!("{header} Cost |")),
        ),
        (
            "its columns in order",
            markdown.replace(
                header,
                &header.replace("| Tokens | Active |", "| Active | Tokens |"),
            ),
        ),
        ("the pipeline lines", without_pipelines),
        ("the round rows", without_rows),
        ("a whole round row: a placeholder", placeholder),
        ("a visible Task cell: an HTML comment", hidden),
        (
            "the totals row",
            markdown.replace(&format!("{total}\n"), ""),
        ),
        ("the whole block", String::new()),
        ("nothing: a second block", format!("{markdown}{markdown}")),
        ("its place outside code", format!("```\n{markdown}```\n")),
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
            "<summary>Round 1 · {}: 2 runs, ",
            task_interrupt::TASK
        )) && markdown.contains(&format!(
            " wall, 1 failed Attempt (1 {}; ",
            task["attempts"]["failures"][0]["class"].as_str().unwrap()
        )),
        "{markdown}"
    );
    // The pipeline line leads, with each step's Worker and the gate's check; the round row
    // has no review to report.
    assert!(
        markdown.contains(
            "\n**fixture/implementation@1.0.0**: implement (command) → gate (pagination) → \
             evaluate (command)\n"
        ),
        "{markdown}"
    );
    assert!(
        markdown.contains(&format!("| 1 | {} | verified | — |", task_interrupt::TASK)),
        "{markdown}"
    );
    assert_eq!(
        value["pipelines"][0]["steps"][1],
        serde_json::json!({"stage": 2, "role": "gate", "nodes": ["root.nodes.check"],
            "checks": ["pagination"]})
    );
    assert!(task.get("findings").is_none(), "{task:#}");
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

/// The document Task of the Provider admission fixture, whose author's Provider fails its call.
/// The author's Worker binds the model `model` spells from the fixture's home directory, or the
/// fixture's `codex-fixture-1` when it spells none. Returns the fixture and what `af task run
/// --execute` printed.
fn failed_provider_task(model: impl FnOnce(&Path) -> Option<String>) -> (Fixture, Value) {
    failed_provider_task_with(
        model,
        " sys.stderr.write('synthetic provider outage\\n'); sys.exit(1)\n",
    )
}

/// `failed_provider_task`, with the fake Codex's author call replaced by `failure`: one line of
/// its Python, indented one space.
fn failed_provider_task_with(
    model: impl FnOnce(&Path) -> Option<String>,
    failure: &str,
) -> (Fixture, Value) {
    let f = Fixture::new(true, 5712, 49152);
    if let Some(model) = model(&f.home) {
        let catalog_path = f.repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let package = f.repo.join(
            catalog["packages"]["builtin/document-author"]["path"]
                .as_str()
                .unwrap(),
        );
        let manifest = package.join("worker.toml");
        let original = std::fs::read_to_string(&manifest).unwrap();
        let fixture_model = "model = \"codex-fixture-1\"";
        assert_eq!(original.matches(fixture_model).count(), 1, "{original}");
        std::fs::write(
            &manifest,
            original.replace(fixture_model, &format!("model = {model:?}")),
        )
        .unwrap();
        catalog["packages"]["builtin/document-author"]["digest"] = toml::Value::String(
            review_config::lock::package_digest("builtin/document-author", &package).unwrap(),
        );
        std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        task_document::commit(&f.repo);
    }
    // The fixture's native Codex answers the capability probe and then fails the author's call.
    let codex = f._root.path().join("bin/codex");
    let native = std::fs::read_to_string(&codex).unwrap();
    let author = " kind='author'; r=json.loads(request)\n";
    assert_eq!(native.matches(author).count(), 1);
    std::fs::write(&codex, native.replace(author, failure)).unwrap();
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
    (f, done)
}

/// `af task report` over the fixture's document Task: the `--json` document, validated against
/// the published schema, or the Markdown block.
fn fixture_report(f: &Fixture, json: bool) -> String {
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
    let text = stdout(&command.output().unwrap());
    if json {
        let value: Value = serde_json::from_str(&text).unwrap();
        schemas::valid(&schemas::validator("task-report-v1.json"), &value);
    }
    text
}

#[test]
fn a_provider_failure_is_counted_with_its_class_and_charge_and_the_provider_stays_private() {
    let (f, done) = failed_provider_task(|_| None);
    let run = |json: bool| fixture_report(&f, json);
    let value: Value = serde_json::from_str(&run(true)).unwrap();
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
    // The pipeline line names the author's Worker and leaves the Provider admission out.
    let steps = &value["pipelines"][0]["steps"];
    assert!(
        steps
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["role"] == "author" && step["worker"]["model"] == "codex-fixture-1"),
        "{value:#}"
    );
    assert!(
        !steps.to_string().contains("providers"),
        "Provider admission is not a step: {value:#}"
    );
    let line = markdown
        .lines()
        .find(|line| line.starts_with("**"))
        .unwrap();
    assert!(
        line.contains("author (codex codex-fixture-1/high)") && !line.contains("provider"),
        "{line}"
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

/// What `af` prints for `args` over the fixture's Store, as text.
fn fixture_text(f: &Fixture, args: &[&str]) -> String {
    stdout(
        &Command::new(AF)
            .current_dir(&f.repo)
            .env("HOME", &f.home)
            .env("PATH", &f.path)
            .env("AF_PROVIDERS_FILE", f.home.join("providers.toml"))
            .args(args)
            .arg("--state")
            .arg(&f.state)
            .output()
            .unwrap(),
    )
}

/// Issue #165, end to end (ADR-0143): the author's Codex call uses a tool and then fails with
/// `Selected model is at capacity`, reporting no usage. Its Attempt settles at zero with its
/// usage unknown and the cause `capacity`, its diagnostic names that cause without copying the
/// Provider's message, and every report shows the usage as unknown rather than as spend, while
/// the admission call's reported usage is charged exactly as before.
#[test]
fn a_capacity_failure_without_usage_is_charged_zero_and_reported_as_unknown() {
    let (f, done) = failed_provider_task_with(
        |_| None,
        concat!(
            " print(json.dumps({'type':'thread.started','thread_id':'synthetic-author'}));",
            " print(json.dumps({'type':'item.completed','item':{'type':'command_execution',",
            "'command':'ls','aggregated_output':'','exit_code':0,'status':'completed'}}));",
            " print(json.dumps({'type':'turn.failed','error':{'message':",
            "'Selected model is at capacity. Please try a different model.'}}));",
            " sys.exit(1)\n",
        ),
    );
    // Only the admission call reported usage, and it is charged exactly that.
    assert_eq!(done["chargeable_tokens"], "5712", "{done:#}");

    let cas = Cas::open_existing(f.state.join("cas")).unwrap();
    let store = EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
    let execution = store
        .task_projection(&cas, "release-notes")
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    let accounting = execution.attempt_accounting();
    let author = accounting
        .iter()
        .find(|a| a.reservation.node == "root.nodes.author" && a.started)
        .expect("the author's Attempt began");
    assert_eq!(author.charged_tokens, 0);
    assert_eq!(
        author.unknown_usage,
        Some(review_core::task::usage::TaskUnknownUsageCauseV1::Capacity)
    );
    assert!(author.usage_id.is_none());
    // Its reservation was released: nothing of it is held or charged.
    assert_eq!(execution.budget.committed_tokens(), 5712);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    drop(store);

    // `af task show --json`: the settlement carries the marker and a closed diagnostic.
    let shown = f.cli(&["task", "show", "release-notes"]);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &shown);
    assert_eq!(shown["unknown_usage_attempts"], 1, "{shown:#}");
    let settled = shown["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["record"]["attempt_id"] == author.attempt_id.as_str()
                && entry["record"]["kind"] == "settled"
        })
        .unwrap();
    assert_eq!(settled["record"]["charged_tokens"], "0");
    assert_eq!(
        settled["record"]["unknown_usage"],
        serde_json::json!({"cause": "capacity"})
    );
    let diagnostic = settled["diagnostic"]["error"].as_str().unwrap();
    assert!(
        diagnostic.contains("Provider model at capacity (capacity)"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("Selected model"), "{diagnostic}");

    // The text of `af task show` and `af task list` never reads the zero as spend.
    let text = fixture_text(&f, &["task", "show", "release-notes"]);
    assert!(
        text.contains("tokens 5712 (+1 unknown: capacity)"),
        "{text}"
    );
    let listed = fixture_text(&f, &["task", "list"]);
    assert!(listed.contains("5712 tokens (+1 unknown)"), "{listed}");
    let listed: Value = serde_json::from_slice(&f.cli(&["task", "list"]).stdout).unwrap();
    schemas::valid(
        &schemas::validator("task-list-entry-v2.json"),
        &listed["tasks"][0],
    );
    assert_eq!(
        listed["tasks"][0]["unknown_usage_attempts"], 1,
        "{listed:#}"
    );

    // `af task report`: the count in the document, the cell and the cause in the block.
    let value: Value = serde_json::from_str(&fixture_report(&f, true)).unwrap();
    let task = &value["tasks"][0];
    assert_eq!(task["chargeable_tokens"], "5712");
    assert_eq!(task["attempts"]["unknown_usage"], 1, "{task:#}");
    assert_eq!(
        task["attempts"]["unknown_usage_causes"],
        serde_json::json!([{"cause": "capacity", "attempts": 1}])
    );
    assert_eq!(value["totals"]["unknown_usage"], 1);
    let node = task["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node"] == "root.nodes.author")
        .unwrap();
    assert_eq!(node["tokens"], "0");
    assert_eq!(node["unknown_usage"], 1);
    let markdown = fixture_report(&f, false);
    let round = markdown
        .lines()
        .find(|line| line.starts_with("| 1 | release-notes |"))
        .unwrap();
    assert!(round.contains(" | 5,712 (+1 unknown) | "), "{round}");
    let total = markdown
        .lines()
        .find(|line| line.starts_with("|  | Total: "))
        .unwrap();
    assert!(total.contains(" | 5,712 (+1 unknown) | "), "{total}");
    assert!(
        markdown.contains(", 1 Attempt's usage unknown (capacity)</summary>"),
        "{markdown}"
    );
    assert!(
        markdown.contains("| root.nodes.author | author | codex codex-fixture-1/high | 1 (1 failed) | 0 (+1 unknown) |"),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
    assert_private(
        &[&markdown, &value.to_string()],
        &["Selected model", "codex-personal", f.home.to_str().unwrap()],
    );
}

/// A model value that is a path, a URL or an account is recorded in the plan as the Worker's
/// model, and the author's Attempt under it fails at the Provider. The report names that
/// Worker's model `unknown` in both forms and carries the value in neither: a path into the
/// machine's home and auth directory, a `file://` URL, a drive-letter path and an email address
/// before a model name, each versioned as the kernel requires of a model ID it binds. The URL
/// and the drive-letter path passed the denylist this rule replaced; the email address passed
/// the rule while it allowed `@`.
#[test]
fn a_path_valued_model_is_reported_as_unknown_even_on_a_failed_attempt() {
    type Spell = fn(&Path) -> String;
    let cases: [(Spell, &[&str]); 4] = [
        (
            |home| format!("{}/.codex/models/gpt-6-sol", home.to_str().unwrap()),
            &[".codex", "models/gpt-6-sol"],
        ),
        (
            |_| "file:///etc/codex/gpt-6-sol".to_owned(),
            &["file:", "/etc/codex", "codex/gpt-6-sol"],
        ),
        (|_| "C:/secrets/gpt-6-sol".to_owned(), &["C:/", "secrets"]),
        (
            |_| "alice@example.com/gpt-6-sol".to_owned(),
            &["alice@example.com", "alice", "@example.com"],
        ),
    ];
    for (spell, parts) in cases {
        let (f, _) = failed_provider_task(|home| Some(spell(home)));
        let model = spell(&f.home);
        assert!(!is_model_identity(&model), "{model}");
        let relative = model.trim_start_matches('/').to_owned();
        // The plan really binds the value: the report is what keeps it out.
        let cas = Cas::open_existing(f.state.join("cas")).unwrap();
        let store = EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
        let projection = store
            .task_projection(&cas, "release-notes")
            .unwrap()
            .unwrap();
        let plan = cas
            .get_json(projection.plan_id.as_deref().unwrap())
            .unwrap();
        assert!(plan.to_string().contains(&model), "{plan}");
        drop(store);

        let value: Value = serde_json::from_str(&fixture_report(&f, true)).unwrap();
        let task = &value["tasks"][0];
        assert_eq!(
            task["attempts"]["failures"][0]["class"], "provider_failure",
            "{task:#}"
        );
        let author = task["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == "root.nodes.author")
            .unwrap();
        assert_eq!(author["failed_attempts"], 1, "{author:#}");
        assert_eq!(
            author["worker"],
            serde_json::json!({"kind":"model","provider_kind":"codex","model":"unknown","effort":"high"})
        );
        let markdown = fixture_report(&f, false);
        assert!(markdown.contains("| codex unknown/high |"), "{markdown}");
        assert_block_is_checked(&markdown);
        let mut forbidden = vec![
            model.as_str(),
            relative.as_str(),
            "codex-personal",
            f.home.to_str().unwrap(),
            f.state.to_str().unwrap(),
        ];
        forbidden.extend_from_slice(parts);
        assert_private(&[&markdown, &value.to_string()], &forbidden);
    }
}

/// Kills the Worker's process group when the test ends, however it ends.
struct WorkerGroup(i32);
impl Drop for WorkerGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(self.0), Signal::SIGKILL);
    }
}

/// `af task start --file ticket.json --execute`, killed outright once its Worker has written
/// its process ID, the leader of its process group, to `ready`: nothing settles that Worker's
/// Attempt, and the Worker's group is killed with it.
fn start_and_kill(af: &dyn Fn(&[&str]) -> Command, ready: &Path) {
    use std::time::{Duration, Instant};
    let mut af = af(&[
        "task",
        "start",
        "--file",
        "ticket.json",
        "--execute",
        "--json",
    ])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    let leader = loop {
        if let Some(pid) = std::fs::read_to_string(ready)
            .ok()
            .and_then(|text| text.split_whitespace().next()?.parse::<i32>().ok())
        {
            break pid;
        }
        if af.try_wait().unwrap().is_some() {
            let out = af.wait_with_output().unwrap();
            panic!(
                "Worker did not start: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert!(Instant::now() < until, "Worker readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    let _group = WorkerGroup(leader);
    af.kill().unwrap();
    af.wait().unwrap();
}

/// `af task refresh` of the interrupted Task, once the dead writer's lease has expired.
fn refresh_once_the_lease_expires(af: &dyn Fn(&[&str]) -> Command) {
    use std::time::{Duration, Instant};
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let refreshed = af(&["task", "refresh", task_interrupt::TASK, "--json"])
            .output()
            .unwrap();
        if refreshed.status.success() {
            break;
        }
        let said = String::from_utf8_lossy(&refreshed.stderr);
        assert!(said.contains("active writer"), "{said}");
        assert!(
            Instant::now() < until,
            "the dead writer's lease never expired"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The interrupted Task's writer leases in order: (epoch, first event, last event, refreshed
/// the source, recorded an execution).
fn writer_leases(store: &EventStore) -> Vec<(u64, u64, u64, bool, bool)> {
    use review_core::task::event::TaskChangeV1;
    let run_id = review_store::store::task::task_run_id(task_interrupt::TASK).unwrap();
    let mut epochs: Vec<(u64, u64, u64, bool, bool)> = Vec::new();
    for event in store.replay(&run_id).unwrap() {
        let transition = review_store::store::task::read_task_transition(&event).unwrap();
        let time = transition.now_unix_ms;
        let index = match epochs.iter().position(|e| e.0 == transition.epoch) {
            Some(index) => index,
            None => {
                epochs.push((transition.epoch, time, time, false, false));
                epochs.len() - 1
            }
        };
        let epoch = &mut epochs[index];
        epoch.1 = epoch.1.min(time);
        epoch.2 = epoch.2.max(time);
        epoch.3 |= matches!(transition.change, TaskChangeV1::SourceRefreshed { .. });
        epoch.4 |= matches!(transition.change, TaskChangeV1::ExecutionRecorded { .. });
    }
    epochs
}

/// `af task start --execute` dies while its implementer's Attempt runs, leaving that Attempt
/// pending; `af task refresh` over a changed issue settles it as abandoned and refreshes the
/// source; `af task run` finishes. The refresh is not a run and its lease time is not active
/// time, and the implementer's Attempts, which ran under the plans before and after the
/// refresh, are charged to the Worker each plan bound.
#[test]
fn a_refresh_that_recovers_a_pending_attempt_is_neither_a_run_nor_active_time() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "pagination");
    let issue = |description: &str| {
        serde_json::json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42",
            "revision":"v1","summary":"Implement offset and limit pagination",
            "description":description,
            "acceptance":{"bounds":"Reject negative or noninteger bounds."}})
        .to_string()
    };
    std::fs::write(repo.join("issue.json"), issue("Preserve the input values.")).unwrap();
    let ticket = repo.join("ticket.json");
    let original = std::fs::read_to_string(&ticket).unwrap();
    let schema = "\"schema\": \"af.task-file/1\",\n";
    assert_eq!(original.matches(schema).count(), 1);
    // The dead writer's lease runs out before the refresh, and the refreshed plan still needs
    // the deadline for its declared Attempts.
    let wall = "\"wall_ms\": 60000,";
    assert_eq!(original.matches(wall).count(), 1);
    std::fs::write(
        &ticket,
        original
            .replacen(
                schema,
                &format!(
                    "{schema}  \"issue\": {{\"kind\": \"local\", \"path\": \"issue.json\"}},\n"
                ),
                1,
            )
            .replacen(wall, "\"wall_ms\": 300000,", 1),
    )
    .unwrap();
    let ready = directory.path().join("worker-ready");
    let resume = directory.path().join("worker-resume");
    task_interrupt::long_running_implementer(&repo, &ready, &resume);

    // Kill af outright while the implementer runs: nothing settles its Attempt.
    let af = |args: &[&str]| {
        let mut command = Command::new(AF);
        command
            .current_dir(&repo)
            .args(args)
            .arg("--state")
            .arg(&state);
        command
    };
    start_and_kill(&af, &ready);

    // Refresh over a changed issue once the dead writer's lease has expired.
    std::fs::write(
        repo.join("issue.json"),
        issue("Preserve every input value, including an empty list."),
    )
    .unwrap();
    refresh_once_the_lease_expires(&af);
    std::fs::write(&resume, b"").unwrap();
    // The refreshed plan is new, so the run confirms it with `--execute`.
    stdout(
        &af(&["task", "run", "--execute", task_interrupt::TASK, "--json"])
            .output()
            .unwrap(),
    );

    // The log has three writer leases: the start, the refresh that recovered the pending
    // Attempt and refreshed the source, and the run.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let epochs = writer_leases(&store);
    assert_eq!(epochs.len(), 3, "{epochs:?}");
    assert!(
        epochs[1].3 && epochs[1].4,
        "the refresh recovered the pending Attempt: {epochs:?}"
    );
    let accounting = store
        .task_projection(&cas, task_interrupt::TASK)
        .unwrap()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting();
    let implementer: Vec<_> = accounting
        .iter()
        .filter(|a| a.started && a.reservation.node == "root.nodes.implement")
        .collect();
    assert_eq!(implementer.len(), 2);
    assert_ne!(
        implementer[0].plan_id, implementer[1].plan_id,
        "the implementer ran under the plans before and after the refresh"
    );
    drop(store);

    let value = document(&repo, &state, &[task_interrupt::TASK]);
    let task = &value["tasks"][0];
    assert_eq!(task["runs"], 2, "the refresh is not a run: {task:#}");
    assert_eq!(
        task["active_ms"].as_u64().unwrap(),
        (epochs[0].2 - epochs[0].1) + (epochs[2].2 - epochs[2].1),
        "the refresh's lease time is not active time: {task:#} {epochs:?}"
    );
    assert_eq!(
        task["attempts"]["failures"],
        serde_json::json!([{
            "class": "abandoned",
            "attempts": 1,
            "tokens": implementer[0].charged_tokens.to_string(),
        }])
    );
    let rows: Vec<&Value> = task["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["node"] == "root.nodes.implement")
        .collect();
    assert_eq!(rows.len(), 1, "both plans bind the same Worker: {task:#}");
    assert_eq!(rows[0]["worker"]["kind"], "command");
    assert_eq!(rows[0]["attempts"], 2);
    assert_eq!(rows[0]["failed_attempts"], 1);
    assert_eq!(
        rows[0]["tokens"],
        (implementer[0].charged_tokens + implementer[1].charged_tokens).to_string()
    );
    let markdown = stdout(&report(&repo, &state, &[task_interrupt::TASK], false));
    assert!(
        markdown.contains(&format!(
            "<summary>Round 1 · {}: 2 runs, ",
            task_interrupt::TASK
        )) && markdown.contains(" wall, 1 failed Attempt (1 abandoned; "),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
}

/// A local Codex substitute: it answers the account probe and the Provider admission call, and
/// holds every other call until it is killed, first writing its process ID and process group to
/// the path its home names as `ready-path`.
const HOLDING_CODEX: &str = r#"#!/usr/bin/python3 -B
import json,os,sys,time
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':'fixture@example.invalid'}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
if request!='Reply with exactly: OK\n':
 ready=open(home+'/ready-path').read()
 open(ready+'.tmp','w').write('%d %d' % (os.getpid(), os.getpgrp()))
 os.rename(ready+'.tmp',ready)
 time.sleep(120)
 sys.exit(1)
usage={'input_tokens':16331,'cached_input_tokens':10624,'output_tokens':5,'reasoning_output_tokens':0,'cache_write_input_tokens':0}
if '-o' in sys.argv:
 with open(sys.argv[sys.argv.index('-o')+1],'w') as f: f.write('OK')
print(json.dumps({'type':'thread.started','thread_id':'synthetic-admission'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'OK'}}))
print(json.dumps({'type':'turn.completed','usage':usage}))
"#;

/// `af task refresh` really binds a node to another Worker between two of its Attempts. The
/// Task prefers `fixture/model-implementation`, whose `implement` node is a Codex model Worker,
/// and falls back to selecting another pipeline. `af task start --execute` dies while that
/// Worker's Attempt runs; the Provider is then removed from the machine, so the refresh that
/// settles the pending Attempt as abandoned can no longer bind the model pipeline and selects
/// `fixture/implementation`, whose `implement` node is a command Worker; `af task run` finishes
/// under it. The report charges each Attempt to the Worker its own plan bound: one row for the
/// model Worker with the abandoned Attempt, one for the command Worker with the Attempt that
/// finished.
#[test]
fn a_refresh_that_binds_a_node_to_another_worker_charges_each_attempt_to_its_own_worker() {
    use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    use review_core::task::pipeline::PipelineDefinitionV1;
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (repo, state) = task_cli::fixture_named(root, "pagination");
    let home = root.join("home");
    let bin = root.join("bin");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&bin).unwrap();
    let ready = root.join("worker-ready");
    std::fs::write(home.join("ready-path"), ready.to_str().unwrap()).unwrap();
    std::fs::write(bin.join("codex"), HOLDING_CODEX).unwrap();
    std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let providers = home.join("providers.toml");
    std::fs::write(
        &providers,
        toml::to_string(&serde_json::json!({"version": 1, "providers": [
            {"id": "codex-personal", "kind": "codex", "auth_dir": home}]}))
        .unwrap(),
    )
    .unwrap();

    // `fixture/model-implementer` is the fixture's implementer bound to a Codex model, and
    // `fixture/model-implementation` the fixture's pipeline with that Worker in its slot.
    let packages = repo.join(".af/task-packages/fixture");
    task_cli::copy_tree(
        &packages.join("implementer"),
        &packages.join("model-implementer"),
    );
    std::fs::remove_file(packages.join("model-implementer/worker.py")).unwrap();
    let manifest = packages.join("model-implementer/worker.toml");
    let mut worker: TaskWorkerManifest =
        toml::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    worker.name = "fixture/model-implementer".into();
    worker.runner = TaskWorkerRunner::Model {
        provider_kind: "codex".into(),
        model: "codex-fixture-1".into(),
        effort: "high".into(),
    };
    worker.signature.attempt.as_mut().unwrap().tokens = 16384;
    worker.signature.attempt.as_mut().unwrap().wall_ms = 180000;
    std::fs::write(&manifest, toml::to_string(&worker).unwrap()).unwrap();
    let definition = packages.join("implementation/pipeline.toml");
    let mut command: PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(&definition).unwrap()).unwrap();
    // The abandoned Attempt keeps its charge, so the finishing run needs a second implementer
    // Attempt within the slot and pipeline limits.
    command.slots.get_mut("implementer").unwrap().max_attempts = 2;
    command.max_attempts = 4;
    std::fs::write(&definition, toml::to_string(&command).unwrap()).unwrap();
    let mut model = command.clone();
    model.name = "fixture/model-implementation".into();
    model.slots.get_mut("implementer").unwrap().worker = "fixture/model-implementer".into();
    std::fs::create_dir(packages.join("model-implementation")).unwrap();
    std::fs::write(
        packages.join("model-implementation/pipeline.toml"),
        toml::to_string(&model).unwrap(),
    )
    .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in ["model-implementer", "model-implementation"] {
        catalog["packages"].as_table_mut().unwrap().insert(
            format!("fixture/{name}"),
            toml::Value::try_from(serde_json::json!({"version": "1.0.0", "digest": "",
                "path": format!(".af/task-packages/fixture/{name}")}))
            .unwrap(),
        );
    }
    catalog.as_table_mut().unwrap().insert(
        "providers".into(),
        toml::Value::try_from(serde_json::json!({"fixture/model-implementer": "codex-personal"}))
            .unwrap(),
    );
    catalog.as_table_mut().unwrap().insert(
        "provider_admission".into(),
        toml::Value::try_from(serde_json::json!({"tokens": 32768, "wall_ms": 45000})).unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let issue = |description: &str| {
        serde_json::json!({"schema":"af.issue-input/1","id":"10042","key":"AF-42",
            "revision":"v1","summary":"Implement offset and limit pagination",
            "description":description,
            "acceptance":{"bounds":"Reject negative or noninteger bounds."}})
        .to_string()
    };
    std::fs::write(repo.join("issue.json"), issue("Preserve the input values.")).unwrap();
    let ticket_path = repo.join("ticket.json");
    let mut ticket: Value =
        serde_json::from_str(&std::fs::read_to_string(&ticket_path).unwrap()).unwrap();
    ticket["issue"] = serde_json::json!({"kind": "local", "path": "issue.json"});
    ticket["pipeline"] =
        serde_json::json!({"name": "fixture/model-implementation", "fallback": "select"});
    ticket["limits"]["tokens"] = 100000.into();
    ticket["limits"]["max_attempts"] = 8.into();
    ticket["limits"]["wall_ms"] = 300000.into();
    std::fs::write(&ticket_path, serde_json::to_vec_pretty(&ticket).unwrap()).unwrap();
    task_interrupt::commit_fixture(&repo);

    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let af = |args: &[&str]| {
        let mut command = Command::new(AF);
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USER", "fixture")
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", &providers)
            .args(args)
            .arg("--state")
            .arg(&state);
        command
    };
    start_and_kill(&af, &ready);
    let held = std::fs::read_to_string(&ready).unwrap();
    let ids: Vec<&str> = held.split_whitespace().collect();
    assert_eq!(
        ids[0], ids[1],
        "the model Worker leads its own process group"
    );

    // The Provider is gone from this machine, and the issue changed.
    std::fs::write(
        &providers,
        toml::to_string(&serde_json::json!({"version": 1, "providers": []})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join("issue.json"),
        issue("Preserve every input value, including an empty list."),
    )
    .unwrap();
    refresh_once_the_lease_expires(&af);
    stdout(
        &af(&["task", "run", "--execute", task_interrupt::TASK, "--json"])
            .output()
            .unwrap(),
    );

    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let epochs = writer_leases(&store);
    assert_eq!(epochs.len(), 3, "start, refresh and run: {epochs:?}");
    let projection = store
        .task_projection(&cas, task_interrupt::TASK)
        .unwrap()
        .unwrap();
    let accounting = projection.execution.as_ref().unwrap().attempt_accounting();
    let implementer: Vec<_> = accounting
        .iter()
        .filter(|a| a.started && a.reservation.node == "root.nodes.implement")
        .collect();
    assert_eq!(implementer.len(), 2, "{accounting:#?}");
    let bound = |plan_id: &str| -> Value {
        let plan = cas.get_json(plan_id).unwrap();
        plan["payload"]["bindings"]["root.slots.implementer"]["execution"].clone()
    };
    // Each Attempt ran under its own plan: the model Worker's before the refresh, abandoned,
    // and the command Worker's after it.
    let attempt = |kind: &str| {
        *implementer
            .iter()
            .find(|a| bound(&a.plan_id)["kind"] == kind)
            .unwrap_or_else(|| panic!("no implementer Attempt under a {kind} Worker"))
    };
    let (under_model, under_command) = (attempt("model"), attempt("command"));
    assert_eq!(bound(&under_model.plan_id)["model"], "codex-fixture-1");
    assert!(matches!(
        under_model.result,
        Some(review_core::task::execution::TaskAttemptResultV1::Abandoned { .. })
    ));
    assert!(matches!(
        under_command.result,
        Some(review_core::task::execution::TaskAttemptResultV1::Succeeded { .. })
    ));
    assert_eq!(
        projection.plan_id.as_deref(),
        Some(under_command.plan_id.as_str()),
        "the refresh selected the command Worker's plan"
    );
    drop(store);

    let value = document(&repo, &state, &[task_interrupt::TASK]);
    let task = &value["tasks"][0];
    assert_eq!(task["pipeline"], "fixture/implementation@1.0.0", "{task:#}");
    assert_eq!(task["runs"], 2, "the refresh is not a run: {task:#}");
    let rows: Vec<&Value> = task["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["node"] == "root.nodes.implement")
        .collect();
    assert_eq!(
        rows.len(),
        2,
        "one row per Worker the node was bound to: {task:#}"
    );
    let row = |kind: &str| {
        *rows
            .iter()
            .find(|row| row["worker"]["kind"] == kind)
            .unwrap()
    };
    let (model_row, command_row) = (row("model"), row("command"));
    assert_eq!(
        model_row["worker"],
        serde_json::json!({"kind":"model","provider_kind":"codex","model":"codex-fixture-1","effort":"high"})
    );
    assert_eq!(model_row["attempts"], 1, "{model_row:#}");
    assert_eq!(model_row["failed_attempts"], 1, "{model_row:#}");
    assert_eq!(model_row["tokens"], under_model.charged_tokens.to_string());
    assert_eq!(command_row["attempts"], 1, "{command_row:#}");
    assert_eq!(command_row["failed_attempts"], 0, "{command_row:#}");
    assert_eq!(
        command_row["tokens"],
        under_command.charged_tokens.to_string()
    );
    assert_eq!(
        task["attempts"]["failures"],
        serde_json::json!([{
            "class": "abandoned",
            "attempts": 1,
            "tokens": under_model.charged_tokens.to_string(),
        }])
    );
    let markdown = stdout(&report(&repo, &state, &[task_interrupt::TASK], false));
    let lines: Vec<&str> = markdown
        .lines()
        .filter(|line| line.starts_with("| root.nodes.implement |"))
        .collect();
    assert_eq!(lines.len(), 2, "{markdown}");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("| codex codex-fixture-1/high | 1 (1 failed) |")),
        "{markdown}"
    );
    assert!(
        lines.iter().any(|line| line.contains("| command | 1 |")),
        "{markdown}"
    );
    assert_block_is_checked(&markdown);
    assert_private(
        &[&markdown, &value.to_string()],
        &[
            "codex-personal",
            "fixture@example.invalid",
            home.to_str().unwrap(),
            state.to_str().unwrap(),
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
        .filter(|line| line.contains(" | gc-"))
        .collect();
    assert_eq!(rows.len(), 2, "{markdown}");
    assert!(rows[0].starts_with("| 1 | gc-newer | "), "{markdown}");
    assert!(rows[1].starts_with("| 2 | gc-older | "), "{markdown}");
    // One pipeline line: both Tasks run the same pipeline under the same bindings.
    assert_eq!(value["pipelines"].as_array().unwrap().len(), 1, "{value:#}");
    assert_eq!(
        markdown
            .lines()
            .filter(|line| line.starts_with("**fixture/plain@1.0.0**: "))
            .count(),
        1,
        "{markdown}"
    );
    assert!(
        markdown.contains(&format!(
            "|  | Total: {} Attempts |  |  | ",
            totals["attempts"]
        )),
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
        markdown
            .lines()
            .any(|line| line.starts_with("| 1 | gc-older | ")
                && line.contains(" (collected) | unknown | ")),
        "{markdown}"
    );
    let pipelines = |markdown: &str| -> Vec<String> {
        markdown
            .lines()
            .filter(|line| line.starts_with("**"))
            .map(str::to_owned)
            .collect()
    };
    // The collected Task's plan is gone: one line says its pipeline is not retained.
    assert_eq!(pipelines(&markdown).len(), 2, "{markdown}");
    assert!(pipelines(&markdown)[0].starts_with("**fixture/plain@1.0.0**: "));
    assert_eq!(pipelines(&markdown)[1], TASK_REPORT_UNKNOWN_PIPELINE);
    assert_block_is_checked(&markdown);

    // Only the collected Task: the block still leads with a pipeline line, and the real
    // checker accepts it.
    let value = document(&repo, &state, &["gc-older"]);
    assert_eq!(value["pipelines"], serde_json::json!([]), "{value:#}");
    let only = stdout(&report(&repo, &state, &["gc-older"], false));
    assert_eq!(pipelines(&only), [TASK_REPORT_UNKNOWN_PIPELINE], "{only}");
    let (passed, said) = check_pr_report(&description(&only));
    assert!(passed, "{said}\n{only}");
    assert_block_is_checked(&only);
}

/// The findings a Task's recorded rounds wrote, read straight from the Store: each round's
/// `FindingSet@1` entries that round saw itself, counted once per finding, by severity.
fn recorded_findings(state: &Path, task_id: &str) -> (u64, [u64; 3]) {
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let run_id = review_store::store::task::task_run_id(task_id).unwrap();
    let mut rounds = Vec::new();
    for event in store.replay(&run_id).unwrap() {
        let transition = review_store::store::task::read_task_transition(&event).unwrap();
        if let review_core::task::event::TaskChangeV1::ExecutionRecorded { record_id } =
            transition.change
        {
            let record =
                review_store::store::task::execution::read_execution_record(&cas, &record_id)
                    .unwrap()
                    .record;
            if let review_core::task::execution::TaskExecutionRecordV1::Published {
                output_id,
                ..
            } = record
            {
                let output = cas.get_json(&output_id).unwrap();
                for port in output["payload"]["outputs"].as_object().unwrap().values() {
                    if port["artifact_type"] == "af/TaskReviewRound@1" {
                        for id in port["artifact_ids"].as_array().unwrap() {
                            if !rounds.contains(id) {
                                rounds.push(id.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    let mut findings = std::collections::BTreeMap::new();
    for id in &rounds {
        let round = cas.get_json(id.as_str().unwrap()).unwrap();
        let Some(set) = round["payload"]["finding_set_id"].as_str() else {
            continue;
        };
        let set = cas.get_json(set).unwrap()["payload"].clone();
        for finding in set["findings"].as_array().unwrap() {
            if finding["last_seen_round"] == set["round"] {
                findings.insert(
                    finding["finding_id"].as_str().unwrap().to_owned(),
                    finding["severity"].as_str().unwrap().to_owned(),
                );
            }
        }
    }
    let count = |severity: &str| findings.values().filter(|s| *s == severity).count() as u64;
    (
        rounds.len() as u64,
        [count("blocker"), count("major"), count("minor")],
    )
}

/// A reviewed implementation whose first Snapshot both reviewers fault: the round's reduce
/// step records the finding, the report counts it once by severity, and the row says so.
#[test]
fn a_review_rounds_findings_are_counted_once_by_severity_from_its_reduce_step() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(directory.path(), "bounded-repair");
    let af = |args: &[&str]| {
        Command::new(AF)
            .current_dir(&repo)
            .args(args)
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap()
    };
    stdout(&af(&["task", "plan", "--file", "ticket.json"]));
    stdout(&af(&["task", "run", "--execute", "repair-cli"]));

    let (rounds, [blocker, major, minor]) = recorded_findings(&state, "repair-cli");
    assert_eq!(rounds, 1);
    assert!(blocker + major + minor > 0, "the round recorded a finding");
    let value = document(&repo, &state, &["repair-cli"]);
    let task = &value["tasks"][0];
    assert_eq!(task["review_rounds"], 1, "{task:#}");
    assert_eq!(
        task["findings"],
        serde_json::json!({"blocker": blocker, "major": major, "minor": minor,
            "review_ran": true, "gate_failed": false, "failed_reviewers": 0}),
        "{task:#}"
    );
    let cell = [(blocker, "blocker"), (major, "major"), (minor, "minor")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, severity)| format!("{n} {severity}"))
        .collect::<Vec<_>>()
        .join(", ");
    let markdown = stdout(&report(&repo, &state, &["repair-cli"], false));
    assert!(
        markdown
            .lines()
            .any(|line| line.starts_with("| 1 | repair-cli | ")
                && line.contains(&format!(" | {cell} | "))),
        "{markdown}"
    );
    // Both reviewers run in one stage of the pipeline line.
    let steps = value["pipelines"][0]["steps"].as_array().unwrap();
    let review = steps
        .iter()
        .find(|step| step["role"] == "review")
        .unwrap_or_else(|| panic!("{value:#}"));
    assert_eq!(review["nodes"].as_array().unwrap().len(), 2, "{value:#}");
    assert_block_is_checked(&markdown);
}

/// Issue #191: `af task refresh` clears the execution outputs, the published review round
/// among them, but not the round the Task recorded. A reviewed implementation finishes one
/// round, its source is refreshed, and it runs again: the report counts the first round
/// before and after the refresh, and both rounds after the second run.
#[test]
fn a_round_recorded_before_a_source_refresh_still_counts() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) =
        crate::task_refresh::setup(root.path(), None, "implementation-reviewed", 10);
    let af = |args: &[&str]| {
        Command::new(AF)
            .current_dir(&repo)
            .args(args)
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap()
    };
    stdout(&af(&[
        "task",
        "start",
        "--execute",
        "--file",
        "ticket.json",
    ]));
    let first = document(&repo, &state, &["issue-refresh"]);
    assert_eq!(first["tasks"][0]["review_rounds"], 1, "{first:#}");
    let findings = first["tasks"][0]["findings"].clone();
    assert_eq!(findings["review_ran"], true, "{first:#}");

    let mut changed = crate::task_refresh::issue();
    changed["revision"] = serde_json::json!("v2");
    changed["acceptance"]["empty"] = serde_json::json!("An empty input returns an empty list.");
    let external = root.path().join("updated.toml");
    let data: review_core::task::source::IssueInputV1 = serde_json::from_value(changed).unwrap();
    std::fs::write(&external, toml::to_string(&data).unwrap()).unwrap();
    stdout(&af(&[
        "task",
        "refresh",
        "issue-refresh",
        "--source-file",
        external.to_str().unwrap(),
    ]));
    // The refresh cleared the outputs: the round is no longer among them.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let execution = store
        .task_projection(&cas, "issue-refresh")
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    assert!(
        execution.outputs.is_empty(),
        "{:?}",
        execution.outputs.keys()
    );
    drop(store);
    let refreshed = document(&repo, &state, &["issue-refresh"]);
    assert_eq!(
        refreshed["tasks"][0]["review_rounds"], 1,
        "the recorded round still counts: {refreshed:#}"
    );
    assert_eq!(refreshed["tasks"][0]["findings"], findings);

    stdout(&af(&["task", "run", "--execute", "issue-refresh"]));
    let second = document(&repo, &state, &["issue-refresh"]);
    let task = &second["tasks"][0];
    assert_eq!(task["review_rounds"], 2, "{task:#}");
    let (rounds, [blocker, major, minor]) = recorded_findings(&state, "issue-refresh");
    assert_eq!(rounds, 2);
    assert_eq!(
        (
            &task["findings"]["blocker"],
            &task["findings"]["major"],
            &task["findings"]["minor"]
        ),
        (
            &serde_json::json!(blocker),
            &serde_json::json!(major),
            &serde_json::json!(minor)
        ),
        "{task:#}"
    );
    assert_eq!(task["runs"], 2, "the refresh is not a run: {task:#}");
    assert_block_is_checked(&stdout(&report(&repo, &state, &["issue-refresh"], false)));
}

/// A duration as the renderer writes it (ADR-0142): `850ms`, `3.2s`, `4m 05s`, `1h 02m`.
fn duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{}.{}s", ms / 1_000, ms % 1_000 / 100),
        60_000..3_600_000 => format!("{}m {:02}s", ms / 60_000, ms % 60_000 / 1_000),
        _ => format!("{}h {:02}m", ms / 3_600_000, ms % 3_600_000 / 60_000),
    }
}

/// A token count as the renderer writes it, with thousands separators: `205,295`.
fn thousands(decimal: &str) -> String {
    let mut out = String::new();
    for (index, digit) in decimal.chars().enumerate() {
        if index > 0 && (decimal.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The cells of the round table's rows, the totals row last.
fn round_table(markdown: &str) -> Vec<Vec<String>> {
    markdown
        .lines()
        .skip_while(|line| !line.starts_with("| ---: |"))
        .skip(1)
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            line[1..line.len() - 1]
                .split(" | ")
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

/// The layout over a real Store: a reviewed implementation whose one round records a major and
/// a minor finding, and a Task on another pipeline without a review. The block leads with the
/// two pipeline lines in first-use order, then one row per Task with its round, outcome,
/// findings, tokens and active time, and the `Total:` row; the real checker accepts it.
#[test]
fn two_pipelines_lead_the_block_in_first_use_order_above_one_row_per_round() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_gc::fixture(root.path());
    // The `bugs` reviewer reports two findings of two severities, and the gate lets both through
    // so the round completes and the Task finishes.
    let worker = repo.join(".af/task-packages/fixture/bugs/worker.py");
    let script = std::fs::read_to_string(&worker).unwrap();
    let reports = "'reports':[{'severity':'major','file':'pagination.py','line':1,\
        'title':'Reject negative offset','body':'A negative offset slices from the end.',\
        'fix':'Raise ValueError for a negative offset.','confidence':0.99},\
        {'severity':'minor','file':'pagination.py','line':2,'title':'Name the default limit',\
        'body':'The default limit 2 is unexplained.','fix':'Name it.','confidence':0.9}]";
    assert!(script.contains("'reports':[]"), "{script}");
    std::fs::write(&worker, script.replace("'reports':[]", reports)).unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["review"]["gate"] = toml::Value::String("blocker".into());
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        pin["digest"] =
            toml::Value::String(review_config::lock::package_digest(name, &package).unwrap());
    }
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        ["add", "-A"].as_slice(),
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "two findings",
        ]
        .as_slice(),
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let started = Command::new(AF)
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            "base.json",
            "--json",
        ])
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    stdout(&started);
    task_gc::start(&repo, &state, "gc-older.json");

    let (rounds, severities) = recorded_findings(&state, "chain-base");
    assert_eq!(
        (rounds, severities),
        (1, [0, 1, 1]),
        "the round's reduce step"
    );
    let ids = ["chain-base", "gc-older"];
    let value = document(&repo, &state, &ids);
    let labels: Vec<String> = value["pipelines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            format!(
                "{}@{}",
                p["name"].as_str().unwrap(),
                p["version"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        labels,
        ["fixture/implementation@1.0.0", "fixture/plain@1.0.0"],
        "{value:#}"
    );
    let markdown = stdout(&report(&repo, &state, &ids, false));
    let lines: Vec<&str> = markdown.lines().collect();
    assert_eq!(lines[..3], [TASK_REPORT_BEGIN, "### af task report", ""]);
    assert!(
        lines[3].starts_with("**fixture/implementation@1.0.0**: implement (command) → "),
        "{markdown}"
    );
    assert!(
        lines[3].contains("review (bugs, correctness: command)"),
        "{markdown}"
    );
    assert_eq!(
        lines[5],
        "**fixture/plain@1.0.0**: implement (command) → gate (pagination) → evaluate (command)",
        "{markdown}"
    );
    assert_eq!(
        lines[7],
        "| Round | Task | Outcome | Findings | Tokens | Active |"
    );

    let tasks = value["tasks"].as_array().unwrap();
    let table = round_table(&markdown);
    assert_eq!(table.len(), 3, "{markdown}");
    for (index, (row, task)) in table.iter().zip(tasks).enumerate() {
        assert_eq!(
            *row,
            [
                (index + 1).to_string(),
                ids[index].to_owned(),
                task["outcome"].as_str().unwrap().to_owned(),
                ["1 major, 1 minor", "—"][index].to_owned(),
                thousands(task["chargeable_tokens"].as_str().unwrap()),
                duration(task["active_ms"].as_u64().unwrap()),
            ],
            "{markdown}"
        );
    }
    assert_eq!(
        tasks[0]["findings"],
        serde_json::json!({"blocker": 0, "major": 1, "minor": 1, "review_ran": true,
            "gate_failed": false, "failed_reviewers": 0}),
        "{value:#}"
    );
    assert!(tasks[1].get("findings").is_none(), "{value:#}");
    let totals = &value["totals"];
    let attempts = totals["attempts"].as_u64().unwrap();
    assert_eq!(totals["failed_attempts"], 0, "{value:#}");
    assert_eq!(
        table[2],
        [
            String::new(),
            format!("Total: {attempts} Attempts"),
            String::new(),
            String::new(),
            thousands(totals["chargeable_tokens"].as_str().unwrap()),
            duration(totals["active_ms"].as_u64().unwrap()),
        ],
        "{markdown}"
    );
    assert_block_is_checked(&markdown);

    // First-use order follows the order the Tasks are named.
    let reversed = stdout(&report(&repo, &state, &["gc-older", "chain-base"], false));
    let pipelines: Vec<&str> = reversed
        .lines()
        .filter(|line| line.starts_with("**"))
        .map(|line| line.split("**: ").next().unwrap())
        .collect();
    assert_eq!(
        pipelines,
        ["**fixture/plain@1.0.0", "**fixture/implementation@1.0.0"],
        "{reversed}"
    );
    assert_block_is_checked(&reversed);
}
