//! `af task report` end to end (ADR-0142), over Stores the existing fixtures build: a Task
//! interrupted and resumed once, a document Task whose model Worker fails at its Provider, and
//! two Tasks of one Store. Every `--json` document validates against
//! `schemas/task-report-v1.json`, every Markdown block passes `scripts/check-pr-report.py`, and
//! neither carries a Provider label, a path or an account the Store or the machine holds.

use nix::sys::signal::Signal;
use review_core::task::task_report::is_model_identity;
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
    let placeholder = markdown.replace(&format!("{}\n", rows[0]), "| x |\n");
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
        (
            "nothing: an extra column",
            markdown.replace(header, &format!("{header} Cost |")),
        ),
        (
            "its columns in order",
            markdown.replace(
                header,
                &header.replace("| Active time | Wall time |", "| Wall time | Active time |"),
            ),
        ),
        ("the Task rows", without_rows),
        ("a whole Task row: a placeholder", placeholder),
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

/// The document Task of the Provider admission fixture, whose author's Provider fails its call.
/// The author's Worker binds the model `model` spells from the fixture's home directory, or the
/// fixture's `codex-fixture-1` when it spells none. Returns the fixture and what `af task run
/// --execute` printed.
fn failed_provider_task(model: impl FnOnce(&Path) -> Option<String>) -> (Fixture, Value) {
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

/// A model value that is a path or a URL is recorded in the plan as the Worker's model, and the
/// author's Attempt under it fails at the Provider. The report names that Worker's model
/// `unknown` in both forms and carries the value in neither: a path into the machine's home and
/// auth directory, a `file://` URL and a drive-letter path, each versioned as the kernel
/// requires of a model ID it binds. The last two passed the denylist this rule replaced.
#[test]
fn a_path_valued_model_is_reported_as_unknown_even_on_a_failed_attempt() {
    type Spell = fn(&Path) -> String;
    let cases: [(Spell, &[&str]); 3] = [
        (
            |home| format!("{}/.codex/models/gpt-6-sol", home.to_str().unwrap()),
            &[".codex", "models/gpt-6-sol"],
        ),
        (
            |_| "file:///etc/codex/gpt-6-sol".to_owned(),
            &["file:", "/etc/codex", "codex/gpt-6-sol"],
        ),
        (|_| "C:/secrets/gpt-6-sol".to_owned(), &["C:/", "secrets"]),
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
            "<summary>{}: 2 runs, 1 failed Attempt (1 abandoned; ",
            task_interrupt::TASK
        )),
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
