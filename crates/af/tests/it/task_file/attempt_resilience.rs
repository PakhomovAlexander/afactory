//! ADR-0202, issue #295, end to end: a fake `claude` whose business call fails with a model at
//! capacity. `af review run` waits inside the Attempt and calls again; the wait is a recorded
//! `TaskProviderEvent@1`, so `af task show` names it while it lasts and `af task show --json`
//! lists it; a failure that outlasts three retries fails the node with its class, and `af task
//! show` prints that class and the diagnostic's first line.
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::task_cli::fixture_named;

/// An answer of the bound model, whose `result` is `text`.
fn answer(text: &str) -> String {
    answer_with(text, None)
}

/// An answer of the bound model, whose `result` is `text` and, for a typed request, whose
/// structured output is `structured`.
fn answer_with(text: &str, structured: Option<Value>) -> String {
    let mut value = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false, "result": text,
        "num_turns": 1, "safety_stops": 0, "stop_reason": "end_turn",
        "usage": {"input_tokens": 3, "output_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        "modelUsage": {"claude-fable-5-1": {"inputTokens": 3, "outputTokens": 1, "cacheCreationInputTokens": 0, "cacheReadInputTokens": 0}}
    });
    if let Some(structured) = structured {
        value["structured_output"] = structured;
    }
    value.to_string()
}

/// The review fixture with both reviewers bound to `claude-fable-5-1` on one Claude Provider
/// whose fake answers the admission probe `OK`, fails business calls with a model at capacity —
/// the first one only, or every one — and answers the rest with an empty review.
struct Fixture {
    _directory: tempfile::TempDir,
    repo: std::path::PathBuf,
    state: std::path::PathBuf,
    home: std::path::PathBuf,
    path: std::ffi::OsString,
    registry: std::path::PathBuf,
}

impl Fixture {
    fn new(always_at_capacity: bool) -> Self {
        use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "review");
        let home = directory.path().join("home");
        let bin = directory.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(home.join("help"), crate::provider_cli::CLAUDE_HELP).unwrap();
        std::fs::write(home.join("version"), "2.1.300 (Claude Code)\n").unwrap();
        std::fs::write(home.join("admitted"), answer("OK")).unwrap();
        std::fs::write(
            home.join("reviewed"),
            answer_with(
                "",
                Some(
                    serde_json::json!({"schema":"af.worker-reply/1","outputs":{"result":[
                        {"reports":[],"benchmark_demands":[],"dispositions":[]}
                    ]}}),
                ),
            ),
        )
        .unwrap();
        if always_at_capacity {
            std::fs::write(home.join("always"), "").unwrap();
        }
        // The first business call — whichever reviewer makes it — and with `always` every one
        // exits 1 having written only Claude Code's overloaded error, as a model at capacity
        // does; `failed` records each.
        let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
if sys.argv[1:] in (['--help'],['--version']):
 sys.stdout.write(open(home+'/'+sys.argv[1][2:]).read())
 sys.exit(0)
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':'developer@example.test'}))
 sys.exit(0)
if '--input-format' in sys.argv:
 sys.exit(0)
request=sys.stdin.read()
if request==PROBE_INPUT:
 sys.stdout.write(open(home+'/admitted').read())
 sys.exit(0)
first=False
try:
 os.close(os.open(home+'/failed-once',os.O_CREAT|os.O_EXCL|os.O_WRONLY)); first=True
except FileExistsError:
 pass
if first or os.path.exists(home+'/always'):
 with open(home+'/failed','a') as f: f.write('capacity\n')
 sys.stderr.write('API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}\n')
 sys.exit(1)
sys.stdout.write(open(home+'/reviewed').read())
"#;
        let installed = bin.join("claude");
        std::fs::write(&installed, crate::provider_cli::with_probe(stub)).unwrap();
        std::fs::set_permissions(&installed, std::fs::Permissions::from_mode(0o755)).unwrap();
        let registry = home.join("providers.toml");
        std::fs::write(&registry,toml::to_string(&serde_json::json!({"version":1,"providers":[{"id":"claude-personal","kind":"claude","auth_dir":home}]})).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let mut providers = toml::map::Map::new();
        for name in ["bugs", "correctness"] {
            let package = format!("fixture/{name}");
            let path = repo.join(format!(".af/task-packages/{package}"));
            let mut worker: TaskWorkerManifest =
                toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap())
                    .unwrap();
            worker.runner = TaskWorkerRunner::Model {
                provider_kind: "claude".into(),
                model: "claude-fable-5-1".into(),
                effort: "xhigh".into(),
            };
            let attempt = worker.signature.attempt.as_mut().unwrap();
            attempt.tokens = 1000;
            attempt.wall_ms = super::NATIVE_REVIEWER_WALL_MS;
            std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
            catalog["packages"][&package]["digest"] =
                toml::Value::String(review_config::lock::package_digest(&package, &path).unwrap());
            providers.insert(package, toml::Value::String("claude-personal".into()));
        }
        let pipeline_dir = repo.join(".af/task-packages/fixture/review");
        let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
            toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
                .unwrap();
        pipeline.max_attempts = 4;
        std::fs::write(
            pipeline_dir.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        catalog["packages"]["fixture/review"]["digest"] = toml::Value::String(
            review_config::lock::package_digest("fixture/review", &pipeline_dir).unwrap(),
        );
        catalog
            .as_table_mut()
            .unwrap()
            .insert("providers".into(), toml::Value::Table(providers));
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let file_path = repo.join("review.json");
        let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
        file["limits"] = super::native_model_limits();
        std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "model bindings"]] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let path = std::env::join_paths(
            std::iter::once(bin.clone())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            _directory: directory,
            repo,
            state,
            home,
            path,
            registry,
        }
    }

    /// `af` in the fixture's checkout, home, `PATH` and registry, against its Store, with the
    /// in-Attempt retry's first wait set to `base_ms`.
    fn af(&self, base_ms: u64) -> Command {
        let mut command = crate::common::af();
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "fixture")
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", &self.registry)
            .env("AF_TEST_PROVIDER_RETRY_BASE_MS", base_ms.to_string());
        command
    }

    fn show(&self, json: bool) -> std::process::Output {
        let mut command = self.af(1);
        command
            .args(["task", "show", "review-cli", "--state"])
            .arg(&self.state);
        if json {
            command.arg("--json");
        }
        command.output().unwrap()
    }

    fn failed_calls(&self) -> usize {
        std::fs::read_to_string(self.home.join("failed"))
            .unwrap_or_default()
            .lines()
            .count()
    }
}

#[test]
fn a_model_at_capacity_is_waited_out_in_its_attempt_and_show_names_the_wait() {
    let fixture = Fixture::new(false);
    let mut run = fixture
        .af(5000)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--json",
            "--state",
        ])
        .arg(&fixture.state)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // While the Attempt waits, `af task show` names the wait, its class and the retry.
    let until = Instant::now() + Duration::from_secs(120);
    let mut seen = None;
    while Instant::now() < until && run.try_wait().unwrap().is_none() {
        let shown = fixture.show(false);
        let stdout = String::from_utf8_lossy(&shown.stdout).into_owned();
        if let Some(line) = stdout
            .lines()
            .find(|line| line.ends_with(": waiting 5 s for capacity (retry 1 of 3)"))
        {
            seen = Some(line.to_owned());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = run.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = seen.unwrap_or_else(|| panic!("no wait was shown\n{stderr}"));
    assert!(line.starts_with("root."), "{line}");
    let human = String::from_utf8_lossy(&fixture.show(false).stdout).into_owned();
    assert!(output.status.success(), "{stderr}\n{human}");
    assert_eq!(fixture.failed_calls(), 1, "{stderr}");
    // The run's result: the retried call completed, and the wait is a recorded event.
    let shown = fixture.show(true);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("task-inspection-v13.json"),
        &shown,
    );
    let events = shown["provider_events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{shown}");
    let record = &events[0]["record"];
    assert_eq!(record["schema"], "af/TaskProviderEvent@1");
    assert_eq!(
        record["event"],
        serde_json::json!({"kind":"retry_wait","class":"capacity","retry":1,"max_retries":3,"wait_ms":5000,"retry_after":false})
    );
    assert!(
        line.starts_with(&format!("{}:", record["node"].as_str().unwrap())),
        "{line}"
    );
    // Once the Attempt settled, nothing waits.
    let after = fixture.show(false);
    assert!(
        !String::from_utf8_lossy(&after.stdout).contains("waiting"),
        "{}",
        String::from_utf8_lossy(&after.stdout)
    );
}

#[test]
fn a_model_still_at_capacity_after_three_retries_fails_its_node_by_class() {
    let fixture = Fixture::new(true);
    let output = fixture
        .af(20)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--json",
            "--state",
        ])
        .arg(&fixture.state)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    // Each reviewer Attempt calls once and retries three times.
    assert!(fixture.failed_calls() >= 4, "{stderr}");
    let shown = fixture.show(true);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    let diagnostics: Vec<&str> = shown["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|record| record["diagnostic"]["error"].as_str())
        .collect();
    assert!(
        diagnostics.iter().any(|error| error.contains(
            "transient Provider failure (capacity) after 3 of 3 retries (retry 1 after capacity \
             waited 20 ms, retry 2 after capacity waited 40 ms, retry 3 after capacity waited \
             80 ms); not retried again: its 3 retries are spent"
        )),
        "{diagnostics:?}"
    );
    let human = fixture.show(false);
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(
        human
            .lines()
            .any(|line| line.contains(": provider_capacity: ")),
        "{human}"
    );
    assert!(
        !human
            .lines()
            .any(|line| line.ends_with(": Required branch receipt was not produced")),
        "a consequence must name what it follows: {human}"
    );
}
