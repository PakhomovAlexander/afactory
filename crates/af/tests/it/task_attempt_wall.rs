//! ADR-0257 end to end: an implementer that writes files and then outlives a short Attempt wall
//! leaves its tree sealed as a partial candidate; the Task ends `exhausted/attempt_wall` with the
//! wall and the elapsed time in `af task show`, its `--json`, `af task report` and the run's
//! stderr; and `af task run NEW --from TASK` continues from those edits, not from the base. The
//! failed Attempt is charged and counted as every failed Attempt is, and spends no recovery
//! allowance.
//!
//! The fixture is `fixtures/task-runtime/continue` with every Worker given the load-safe wall
//! but the implementer, whose wall is short for the first Task and load-safe for the second.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::task_cli;

/// The implementer's wall in the first Task: long enough for a loaded machine to start Python
/// and write two files, far shorter than the ten minutes the Worker then sleeps.
const SHORT_WALL_MS: i64 = 5_000;

const IMPLEMENTER: &str = r#"import json,os,sys,time
request=json.load(sys.stdin)
if os.path.exists('wip.txt'):
    # A continuation starts from the partial tree the Attempt wall left, never from the base.
    source=open('pagination.py').read()
    assert 'offset:offset+limit' in source, source
    os.remove('wip.txt')
    open('NOTES.md','w').write('paginate(items, offset, limit) returns one page.\n')
    print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Finished the partial tree'}]}}))
else:
    open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\n    return items[offset:offset+limit]\n')
    open('wip.txt','w').write('half done\n')
    time.sleep(600)
"#;

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap()
}

fn commit(repo: &Path, message: &str) {
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", message].as_slice(),
    ] {
        let output = git(repo, args);
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "git {args:?}: {err}");
    }
}

fn run(repo: &Path, state: &Path, args: &[&str], json: bool) -> Output {
    let mut command = crate::common::af();
    command.current_dir(repo).args(args);
    if json {
        command.arg("--json");
    }
    command.arg("--state").arg(state).output().unwrap()
}

/// The one JSON document a command printed, its exit code checked, and its stderr.
fn json(repo: &Path, state: &Path, args: &[&str], expected: i32) -> (Value, String) {
    let output = run(repo, state, args, true);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    let shown = format!("{args:?}\n{out}\n{err}");
    assert_eq!(output.status.code(), Some(expected), "{shown}");
    (serde_json::from_str(&out).expect(&shown), err)
}

/// What a command printed on stdout, its exit code checked.
fn text(repo: &Path, state: &Path, args: &[&str], expected: i32) -> String {
    let output = run(repo, state, args, false);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{args:?}\n{out}\n{err}"
    );
    out
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn valid(name: &str, value: &Value) {
    let directory = workspace().join("schemas");
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let Some(id) = schema["$id"].as_str().map(str::to_owned) else {
            continue;
        };
        registry = registry
            .add(id, jsonschema::Resource::from_contents(schema))
            .unwrap();
    }
    let schema: Value =
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap();
    let registry = registry.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&schema)
        .unwrap();
    let errors: Vec<_> = validator
        .iter_errors(value)
        .map(|e| format!("{} at {}", e, e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{name}: {}", errors.join("\n"));
}

/// Pin every package digest of the copied tree and commit it.
fn pin(repo: &Path, message: &str) {
    let path = repo.join(".af/task-catalog.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut catalog: toml::Value = toml::from_str(&source).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        let digest = review_config::lock::package_digest(name, &package);
        pin["digest"] = toml::Value::String(digest.unwrap());
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    commit(repo, message);
}

/// Give the implementer `wall_ms` as its Attempt wall.
fn implementer_wall(repo: &Path, wall_ms: i64) {
    let path = repo.join(".af/task-packages/fixture/implementer/worker.toml");
    let mut worker: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    worker["signature"]["attempt"]["wall_ms"] = toml::Value::Integer(wall_ms);
    std::fs::write(&path, toml::to_string(&worker).unwrap()).unwrap();
}

/// The payload of one recorded artifact.
fn payload(cas: &review_store::Cas, id: &Value) -> Value {
    cas.get_json(id.as_str().unwrap()).unwrap()["payload"].clone()
}

/// The bytes of `path` in the Snapshot an `af/SourceTree@1` artifact names.
fn file_in(cas: &review_store::Cas, source_tree: &Value, path: &str) -> Option<Vec<u8>> {
    let snapshot = payload(cas, source_tree)["snapshot_id"].clone();
    let (_, manifest) =
        review_source_git::task::read_snapshot(cas, snapshot.as_str().unwrap()).unwrap();
    let entry = manifest.get(path)?;
    Some(cas.get(&entry.content).unwrap())
}

#[test]
fn an_attempt_wall_keeps_the_implementers_edits_and_a_continuation_starts_from_them() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(root.path(), "continue");
    task_cli::load_safe_walls(&repo);
    implementer_wall(&repo, SHORT_WALL_MS);
    std::fs::write(
        repo.join(".af/task-packages/fixture/implementer/worker.py"),
        IMPLEMENTER,
    )
    .unwrap();
    let file = repo.join("first.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    task_cli::load_safe_limits(&mut task["limits"]);
    std::fs::write(&file, serde_json::to_vec_pretty(&task).unwrap()).unwrap();
    pin(&repo, "a short implementer wall");

    // The plan says, before anything runs, how long one implementer Attempt may take.
    let preview = text(&repo, &state, &["task", "plan", "--file", "first.json"], 0);
    assert!(
        preview.contains("  implementer: command Worker, Attempt wall 5 s"),
        "{preview}"
    );
    let (planned, _) = json(&repo, &state, &["task", "plan", "--file", "first.json"], 0);
    valid("task-inspection-v14.json", &planned);
    let implementer = planned["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|slot| slot["slot"].as_str().unwrap().ends_with("implementer"))
        .expect("the implementer slot");
    assert_eq!(implementer["attempt_wall_ms"], SHORT_WALL_MS, "{planned:#}");
    assert_eq!(implementer["execution"], "command");
    assert_eq!(implementer["worker"], "fixture/implementer");

    // The implementer writes two files and sleeps past its wall: exhausted, never a malformed
    // reply or a Provider failure.
    let plan_id = planned["plan_id"].as_str().unwrap().to_owned();
    let start = ["task", "run", "first", "--confirm-plan", &plan_id];
    let (first, err) = json(&repo, &state, &start, 4);
    valid("task-inspection-v14.json", &first);
    assert_eq!(first["state"]["phase"], "finished", "{first:#}");
    assert_eq!(first["state"]["outcome"], "exhausted", "{first:#}");
    assert_eq!(first["state"]["cause"], "attempt_wall", "{first:#}");
    let next = first["state"]["next_action"].as_str().unwrap();
    assert!(
        next.starts_with("af task run NEW_TASK_ID --from first"),
        "{next}"
    );
    let failure = &first["failures"][0];
    assert_eq!(failure["node"], "root.nodes.implement", "{first:#}");
    assert_eq!(failure["cause"], "attempt_wall");
    let line = failure["first_line"].as_str().unwrap();
    assert!(
        line.starts_with("implementer reached its Attempt wall of 5 s after ")
            && line.contains("are kept as partial candidate sha256:"),
        "{line}"
    );
    assert!(
        err.contains("af task run: implement failed: its Attempt wall of 5 s ran out after")
            && err.contains("its edits are kept as a partial candidate"),
        "{err}"
    );
    assert!(
        err.trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("Task first: finished/exhausted/attempt_wall; next: af task run NEW_TASK_ID --from first"),
        "{err}"
    );

    // The wall end and the partial tree it names are typed records of the failed Attempt.
    let ends = first["attempt_wall_ends"].as_array().expect("wall ends");
    assert_eq!(ends.len(), 1, "{first:#}");
    let end = &ends[0]["record"];
    valid("task-attempt-wall-end-v1.json", end);
    assert_eq!(end["task_id"], "first");
    assert_eq!(end["node"], "root.nodes.implement");
    assert_eq!(end["wall_ms"], SHORT_WALL_MS);
    assert!(end["elapsed_ms"].as_u64().unwrap() > 0, "{end}");
    let partial = &ends[0]["partial_candidate"];
    valid("task-partial-candidate-v1.json", partial);
    assert_eq!(partial["changed_paths"], 2, "{partial}");
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let first_revision = payload(&cas, &first["revision_id"]);
    assert_eq!(
        partial["parent_snapshot_id"], first_revision["inputs"]["source"]["snapshot_id"],
        "the partial tree is derived from the Attempt's own source"
    );
    let kept = file_in(&cas, &partial["source_tree_id"], "pagination.py").unwrap();
    assert!(
        String::from_utf8(kept)
            .unwrap()
            .contains("offset:offset+limit")
    );
    assert!(file_in(&cas, &partial["source_tree_id"], "wip.txt").is_some());

    // Accounting is as for every failed Attempt: one begun Attempt, settled `failed` (never
    // abandoned, so no recovery allowance is spent), charged what its transport reported (a
    // command Worker reports zero tokens), and its raw evidence holds the wall end and the
    // partial tree.
    assert_eq!(first["attempts"], 1, "{first:#}");
    assert_eq!(first["chargeable_tokens"], "0");
    let settled: Vec<&Value> = first["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["record"]["kind"] == "settled")
        .collect();
    assert_eq!(settled.len(), 1, "{first:#}");
    let record = &settled[0]["record"];
    assert_eq!(record["result"]["kind"], "failed", "{record}");
    assert_eq!(record["charged_tokens"], "0");
    assert!(record.get("unknown_usage").is_none(), "{record}");
    let raw = record["raw_artifact_ids"].as_array().unwrap();
    assert!(raw.contains(&ends[0]["artifact_id"]), "{record}");
    assert!(raw.contains(&end["partial_candidate_id"]), "{record}");
    assert_eq!(settled[0]["diagnostic"]["cause"], "attempt_wall");
    // Nothing was accepted or verified: the result holds no Snapshot to deliver.
    assert!(
        first["result"]["outputs"].get("snapshot").is_none(),
        "{first:#}"
    );
    assert_ne!(first["result"]["acceptance"], "satisfied");

    // `af task show` and `af task report` say it in the same words.
    let shown = text(&repo, &state, &["task", "show", "first"], 0);
    assert!(
        shown.contains(
            "root.nodes.implement: attempt_wall: implementer reached its Attempt wall of 5 s after"
        ),
        "{shown}"
    );
    assert!(
        shown.contains("next: af task run NEW_TASK_ID --from first"),
        "{shown}"
    );
    let report = text(&repo, &state, &["task", "report", "first"], 0);
    let row = report
        .lines()
        .find(|line| line.contains("| first |"))
        .expect(&report);
    assert!(
        row.contains("attempt_wall")
            && row.contains("implementer reached its Attempt wall of 5 s after"),
        "{report}"
    );
    // The report says how to continue, and never counts the wall end as a Provider failure.
    assert!(
        row.contains("next: af task run NEW_TASK_ID --from first"),
        "{report}"
    );
    assert!(!report.contains("provider_failure"), "{report}");
    let (reported, _) = json(&repo, &state, &["task", "report", "first"], 0);
    valid("task-report-v3.json", &reported);
    let failures = &reported["tasks"][0]["attempts"]["failures"];
    assert_eq!(
        failures,
        &serde_json::json!([{"class": "attempt_wall", "attempts": 1, "tokens": "0"}]),
        "{reported:#}"
    );

    // The continuation runs with a load-safe implementer wall and starts from the kept edits.
    implementer_wall(&repo, task_cli::LOAD_SAFE_WALL_MS);
    pin(&repo, "a load-safe implementer wall");
    // No review ran, so there is no round to name; the refusal says what to do instead.
    let wrong = run(
        &repo,
        &state,
        &["task", "run", "second", "--from", "first", "--round", "1"],
        false,
    );
    let err = String::from_utf8_lossy(&wrong.stderr).into_owned();
    assert_eq!(wrong.status.code(), Some(1), "{err}");
    assert!(
        err.contains("recorded no review round, so it has no round 1")
            && err.contains("omit --round"),
        "{err}"
    );
    let plan = ["task", "run", "second", "--from", "first"];
    let (planned, _) = json(&repo, &state, &plan, 0);
    valid("task-inspection-v14.json", &planned);
    let continues = &planned["continues"];
    assert_eq!(continues["task_id"], "first", "{planned:#}");
    assert_eq!(continues["node"], "root.nodes.implement");
    assert_eq!(continues["wall_end_id"], ends[0]["artifact_id"]);
    assert_eq!(
        continues["partial_candidate_id"],
        end["partial_candidate_id"]
    );
    assert_eq!(continues["snapshot_id"], partial["snapshot_id"]);
    let revision = payload(&cas, &planned["revision_id"]);
    assert!(
        revision["inputs"].get("findings").is_none(),
        "no review ran, so no findings travel"
    );
    let source = &revision["inputs"]["source"]["artifact_ids"][0];
    assert!(
        file_in(&cas, source, "wip.txt").is_some(),
        "the new Task's source is the partial tree, not the base"
    );
    let plan_id = planned["plan_id"].as_str().unwrap().to_owned();
    let confirm = ["task", "run", "second", "--confirm-plan", &plan_id];
    let (second, _) = json(&repo, &state, &confirm, 0);
    assert_eq!(second["result"]["acceptance"], "satisfied", "{second:#}");
    let sealed = &second["result"]["outputs"]["snapshot"]["artifact_ids"][0];
    assert!(file_in(&cas, sealed, "NOTES.md").is_some());
    assert!(file_in(&cas, sealed, "wip.txt").is_none());
    let shown = text(&repo, &state, &["task", "show", "second"], 0);
    assert!(
        shown.contains(
            "continues first from the partial tree its node root.nodes.implement left at its \
             Attempt wall"
        ),
        "{shown}"
    );
    let report = text(&repo, &state, &["task", "report", "first", "second"], 0);
    let row = report
        .lines()
        .find(|line| line.contains("| second |"))
        .expect(&report);
    assert!(
        row.contains("continues first from its partial tree"),
        "{report}"
    );
}
