//! ADR-0127 end to end: `af task list --sizes` and `af task gc` over real Tasks in one Store.
//!
//! Two finished Tasks, `--keep 1`: the preview writes nothing, `--apply` tombstones the older
//! one and removes exactly the bytes the preview named, the retained Task's `show --json` is
//! byte-identical, and the collected Task is listed and shown as `collected` while its outputs
//! and its delivery are refused with the same word. A bound Task is refused by name, a sweep
//! stopped after the tombstone is finished by the next run, and a Campaign's records keep every
//! object they reach.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use review_store::Cas;

#[path = "support/task_cli.rs"]
mod task_cli;

#[path = "support/schemas.rs"]
mod schemas;

const AF: &str = env!("CARGO_BIN_EXE_af");

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap()
}

fn af(repo: &Path, state: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(AF);
    command.current_dir(repo).args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.arg("--state").arg(state).output().unwrap()
}

fn json_of(repo: &Path, state: &Path, args: &[&str], expected: i32) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    let output = af(repo, state, &args, &[]);
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{args:?}\n{out}\n{err}"
    );
    serde_json::from_str(&out).unwrap()
}

fn raw_json(repo: &Path, state: &Path, args: &[&str]) -> String {
    let mut args = args.to_vec();
    args.push("--json");
    let output = af(repo, state, &args, &[]);
    assert_eq!(output.status.code(), Some(0), "{args:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(repo: &Path, state: &Path, args: &[&str]) -> String {
    let output = af(repo, state, args, &[]);
    let err = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "{args:?}\n{err}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The fixture pins its package digests from the copied tree, as `task_input_bindings.rs` does.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, state) = task_cli::fixture_named(root, "bound-inputs");
    let path = repo.join(".af/task-catalog.toml");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut catalog: toml::Value = toml::from_str(&source).unwrap();
    for (name, pin) in catalog["packages"].as_table_mut().unwrap() {
        let package = repo.join(pin["path"].as_str().unwrap());
        let digest = review_config::lock::package_digest(name, &package);
        pin["digest"] = toml::Value::String(digest.unwrap());
    }
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    for (task_id, goal) in [
        ("gc-older", "Implement offset/limit pagination"),
        ("gc-newer", "Implement offset/limit pagination again"),
    ] {
        let file = json!({"schema":"af.task-file/1","task_id":task_id,"kind":"implement",
            "goal":goal,"pipeline":{"name":"fixture/plain","fallback":"refuse"},
            "strategy":"small","facts":{},
            "limits":{"tokens":1000,"max_attempts":3,"wall_ms":60000,
                "verification":{"tokens":200,"attempts":2,"wall_ms":10000}}});
        std::fs::write(
            repo.join(format!("{task_id}.json")),
            serde_json::to_vec_pretty(&file).unwrap(),
        )
        .unwrap();
    }
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "pin packages"].as_slice(),
    ] {
        let output = git(&repo, args);
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(output.status.success(), "git {args:?}: {err}");
    }
    (repo, state)
}

fn start(repo: &Path, state: &Path, file: &str) -> Value {
    let done = json_of(
        repo,
        state,
        &["task", "start", "--execute", "--file", file],
        0,
    );
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    // Distinct last-event times order the Tasks.
    std::thread::sleep(std::time::Duration::from_millis(20));
    done
}

/// Every file below `root` with its bytes: a preview must leave all of them as they were.
fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

fn store_bytes(repo: &Path, state: &Path) -> u64 {
    let listed = json_of(repo, state, &["task", "list", "--sizes"], 0);
    listed["store"]["bytes"].as_u64().unwrap()
}

fn entry<'a>(document: &'a Value, task_id: &str) -> &'a Value {
    document["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["task_id"] == task_id)
        .unwrap_or_else(|| panic!("{task_id} in {document}"))
}

fn source_snapshot(cas: &Cas, revision_id: &str) -> String {
    let revision = cas.get_json(revision_id).unwrap()["payload"].clone();
    revision["inputs"]["source"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn keeping_one_collects_the_older_task_and_removes_exactly_the_previewed_bytes() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let older = start(&repo, &state, "gc-older.json");
    start(&repo, &state, "gc-newer.json");

    // Sizes: each Task's own bytes and the bytes it shares — the source Snapshot, at least.
    let sized = json_of(&repo, &state, &["task", "list", "--sizes"], 0);
    let entries = schemas::validator("task-list-entry-v2.json");
    for task in sized["tasks"].as_array().unwrap() {
        schemas::valid(&entries, task);
        assert!(
            task["sizes"]["exclusive_bytes"].as_u64().unwrap() > 0,
            "{task}"
        );
        assert!(
            task["sizes"]["shared_bytes"].as_u64().unwrap() > 0,
            "{task}"
        );
    }
    assert!(sized["store"]["objects"].as_u64().unwrap() > 0);
    let listed = text(&repo, &state, &["task", "list", "--sizes"]);
    assert!(listed.contains("bytes exclusive"), "{listed}");
    assert!(listed.contains("Store: "), "{listed}");
    let plain = json_of(&repo, &state, &["task", "list"], 0);
    assert!(
        plain["tasks"][0].get("sizes").is_none() && plain.get("store").is_none(),
        "without --sizes the list is what it was"
    );

    // The preview writes nothing and says what it would do.
    let before = tree(&state);
    let preview = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1"],
        0,
    );
    schemas::valid(&schemas::validator("task-gc-v1.json"), &preview);
    assert_eq!(tree(&state), before, "gc without --apply wrote something");
    assert_eq!(preview["apply"], false);
    assert!(preview["applied"].is_null());
    assert_eq!(
        entry(&preview, "gc-older")["disposition"]["kind"],
        "collect"
    );
    assert_eq!(
        entry(&preview, "gc-newer")["disposition"]["kind"],
        "kept_newest"
    );
    let reclaimable = preview["reclaimable_bytes"].as_u64().unwrap();
    let collected_bytes = entry(&preview, "gc-older")["collected_bytes"]
        .as_u64()
        .unwrap();
    assert!(collected_bytes > 0 && collected_bytes <= reclaimable);
    let shown_preview = text(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1"],
    );
    assert!(
        shown_preview.contains("gc-older  collect"),
        "{shown_preview}"
    );
    assert!(
        shown_preview.contains("gc-newer  kept: among the newest 1"),
        "{shown_preview}"
    );
    assert!(
        shown_preview.contains("Nothing was written"),
        "{shown_preview}"
    );
    assert_eq!(tree(&state), before, "a text preview wrote something");

    let retained_before = raw_json(&repo, &state, &["task", "show", "gc-newer"]);
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let older_revision = older["revision_id"].as_str().unwrap().to_owned();
    let older_result = older["phase"]["result_id"].as_str().unwrap().to_owned();
    let shared = source_snapshot(&cas, &older_revision);
    let bytes_before = store_bytes(&repo, &state);

    let applied = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1", "--apply"],
        0,
    );
    schemas::valid(&schemas::validator("task-gc-v1.json"), &applied);
    assert_eq!(applied["applied"]["tombstoned"], json!(["gc-older"]));
    assert_eq!(applied["applied"]["swept"], true);
    assert_eq!(
        applied["reclaimable_bytes"], reclaimable,
        "the plan did not move"
    );
    assert_eq!(applied["applied"]["removed_bytes"], reclaimable);
    assert_eq!(
        bytes_before - store_bytes(&repo, &state),
        reclaimable,
        "the Store shrank by exactly the previewed bytes"
    );

    // Its exclusive objects are gone; the shared ones and the retained Task are untouched.
    assert!(!cas.is_filed(&older_revision));
    assert!(!cas.is_filed(&older_result));
    assert!(cas.is_filed(&shared), "the shared source Snapshot stays");
    let retained_after = raw_json(&repo, &state, &["task", "show", "gc-newer"]);
    assert_eq!(
        retained_after, retained_before,
        "the retained Task's show changed"
    );

    // Listed and shown as collected, with the retained summary.
    let listed = json_of(&repo, &state, &["task", "list"], 0);
    let gone = entry(&listed, "gc-older");
    schemas::valid(&entries, gone);
    assert_eq!(gone["collected"]["schema"], "af/TaskCollected@1");
    assert_eq!(gone["collected"]["revision_id"], older_revision.as_str());
    assert_eq!(gone["collected"]["collected_bytes"], collected_bytes);
    assert_eq!(gone["phase"]["result_id"], older_result.as_str());
    assert_eq!(gone["outcome"], older["result"]["domain_conclusion"]);
    // Under --sizes a collected Task reaches nothing, and its row says so in numbers.
    let sized = json_of(&repo, &state, &["task", "list", "--sizes"], 0);
    let gone_sized = entry(&sized, "gc-older");
    schemas::valid(&entries, gone_sized);
    assert_eq!(
        gone_sized["sizes"],
        serde_json::json!({"exclusive_objects": 0, "exclusive_bytes": 0, "shared_objects": 0, "shared_bytes": 0})
    );
    assert!(
        entry(&sized, "gc-newer")["sizes"]["exclusive_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let line = text(&repo, &state, &["task", "list"]);
    assert!(
        line.lines()
            .any(|row| row.starts_with("gc-older") && row.contains("collected 20")),
        "{line}"
    );
    let shown = json_of(&repo, &state, &["task", "show", "gc-older"], 0);
    schemas::valid(
        &schemas::validator("task-collected-inspection-v1.json"),
        &shown,
    );
    assert_eq!(shown["collected"], gone["collected"]);
    let shown = text(&repo, &state, &["task", "show", "gc-older"]);
    assert!(shown.starts_with("Task gc-older: collected 20"), "{shown}");
    assert!(shown.contains(&older_revision), "{shown}");

    // Its outputs and its delivery are refused with the same word.
    let written = root.path().join("snapshot.json");
    let output = json_of(
        &repo,
        &state,
        &[
            "task",
            "output",
            "gc-older",
            "--port",
            "snapshot",
            "--output",
            written.to_str().unwrap(),
        ],
        1,
    );
    assert!(
        output["error"].as_str().unwrap().contains("collected"),
        "{output}"
    );
    assert!(!written.exists());
    let worktree = root.path().join("delivered");
    let delivered = json_of(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            "gc-older",
            "--confirm",
            "gc-older",
            "--branch",
            "gc/older",
            "--worktree",
            worktree.to_str().unwrap(),
        ],
        1,
    );
    assert!(
        delivered["error"].as_str().unwrap().contains("collected"),
        "{delivered}"
    );
    assert!(!worktree.exists());

    // A second collection has nothing left to do and removes nothing.
    let again = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1", "--apply"],
        0,
    );
    assert_eq!(again["applied"]["tombstoned"], json!([]));
    assert_eq!(again["applied"]["removed_bytes"], 0);
    assert_eq!(again["collected"][0]["task_id"], "gc-older");
}

#[test]
fn a_bound_task_is_never_collected_and_the_preview_names_its_binder() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    start(&repo, &state, "base.json");
    start(&repo, &state, "successor-history.json");
    let planned = json_of(
        &repo,
        &state,
        &["task", "plan", "--file", "gc-newer.json"],
        0,
    );
    assert_ne!(planned["phase"]["kind"], "finished");

    let preview = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "0"],
        0,
    );
    schemas::valid(&schemas::validator("task-gc-v1.json"), &preview);
    assert_eq!(
        entry(&preview, "chain-base")["disposition"],
        json!({"kind": "bound_by", "tasks": ["chain-history"]})
    );
    assert_eq!(
        entry(&preview, "chain-history")["disposition"]["kind"],
        "collect"
    );
    assert_eq!(
        entry(&preview, "gc-newer")["disposition"]["kind"],
        "unfinished"
    );
    let shown = text(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "0"],
    );
    assert!(
        shown.contains("chain-base  never collected: bound by chain-history"),
        "{shown}"
    );
    assert!(
        shown.contains("gc-newer  never collected: not finished"),
        "{shown}"
    );

    let applied = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "0", "--apply"],
        0,
    );
    assert_eq!(applied["applied"]["tombstoned"], json!(["chain-history"]));
    // Once its binder is collected, nothing names the base any more.
    let next = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "0"],
        0,
    );
    assert_eq!(entry(&next, "chain-base")["disposition"]["kind"], "collect");
    let kept = json_of(&repo, &state, &["task", "show", "chain-base"], 0);
    assert_eq!(kept["schema"], "af/task-inspection@11");
}

#[test]
fn a_sweep_stopped_after_the_tombstone_is_finished_by_the_next_run() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let older = start(&repo, &state, "gc-older.json");
    start(&repo, &state, "gc-newer.json");
    let preview = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1"],
        0,
    );
    let reclaimable = preview["reclaimable_bytes"].as_u64().unwrap();
    let bytes_before = store_bytes(&repo, &state);

    let args = [
        "task",
        "gc",
        "--older-than",
        "0",
        "--keep",
        "1",
        "--apply",
        "--json",
    ];
    let stopped = af(
        &repo,
        &state,
        &args,
        &[("AF_TEST_GC_STOP_AFTER_TOMBSTONES", "1")],
    );
    assert_eq!(stopped.status.code(), Some(0));
    let stopped: Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(stopped["applied"]["tombstoned"], json!(["gc-older"]));
    assert_eq!(stopped["applied"]["swept"], false);
    // Tombstoned with every object still present: consistent, never corrupt.
    assert_eq!(store_bytes(&repo, &state), bytes_before);
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    assert!(cas.is_filed(older["revision_id"].as_str().unwrap()));
    let shown = json_of(&repo, &state, &["task", "show", "gc-older"], 0);
    assert_eq!(shown["schema"], "af/task-collected-inspection@1");
    let listed = json_of(&repo, &state, &["task", "list"], 0);
    assert!(entry(&listed, "gc-older")["collected"].is_object());

    let finished = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1", "--apply"],
        0,
    );
    assert_eq!(
        finished["applied"]["tombstoned"],
        json!([]),
        "no second tombstone"
    );
    assert_eq!(finished["applied"]["swept"], true);
    assert_eq!(finished["applied"]["removed_bytes"], reclaimable);
    assert_eq!(bytes_before - store_bytes(&repo, &state), reclaimable);
    assert!(!cas.is_filed(older["revision_id"].as_str().unwrap()));
}

/// Every artifact a record of `runs` references, straight from the log.
fn referenced(state: &Path, runs: &str) -> std::collections::BTreeSet<String> {
    let connection = rusqlite::Connection::open(state.join("events.sqlite")).unwrap();
    let mut query = connection
        .prepare(&format!("SELECT artifact_refs FROM events WHERE {runs}"))
        .unwrap();
    let rows = query.query_map([], |row| row.get::<_, String>(0)).unwrap();
    rows.flat_map(|row| serde_json::from_str::<Vec<String>>(&row.unwrap()).unwrap())
        .collect()
}

#[test]
fn a_campaign_keeps_every_object_its_records_reach_when_its_task_is_collected() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(root.path());
    let older = start(&repo, &state, "gc-older.json");
    start(&repo, &state, "gc-newer.json");
    let result = older["phase"]["result_id"].as_str().unwrap().to_owned();
    let revision = older["revision_id"].as_str().unwrap().to_owned();
    // A Campaign run in the same Store whose record reaches the older Task's result — and,
    // through the result's envelope, the revision it names.
    {
        let cas = Cas::open_existing(state.join("cas")).unwrap();
        let mut store = review_store::EventStore::open(state.join("events.sqlite")).unwrap();
        store
            .append(
                "campaign-keeps",
                &cas,
                review_store::NewEvent::new(review_core::EventType::SourceCapturedV1, json!({}))
                    .referencing(vec![result.clone()]),
            )
            .unwrap();
    }
    let campaign = referenced(&state, "run_id NOT LIKE 'task:%'");
    assert_eq!(campaign, [result.clone()].into());
    let preview = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1"],
        0,
    );
    let sizes = &entry(&preview, "gc-older")["sizes"];
    assert!(sizes["shared_objects"].as_u64().unwrap() > 0);

    let bytes_before = store_bytes(&repo, &state);
    let applied = json_of(
        &repo,
        &state,
        &["task", "gc", "--older-than", "0", "--keep", "1", "--apply"],
        0,
    );
    assert_eq!(applied["applied"]["tombstoned"], json!(["gc-older"]));
    assert_eq!(
        bytes_before - store_bytes(&repo, &state),
        preview["reclaimable_bytes"].as_u64().unwrap()
    );
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    assert!(
        cas.is_filed(&result),
        "the Campaign record still reaches it"
    );
    assert!(
        cas.is_filed(&revision),
        "and what the result's envelope names"
    );
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    assert_eq!(store.replay("campaign-keeps").unwrap().len(), 1);
    let shown = json_of(&repo, &state, &["task", "show", "gc-older"], 0);
    assert_eq!(shown["schema"], "af/task-collected-inspection@1");
}
