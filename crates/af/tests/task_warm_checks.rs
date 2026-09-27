//! Warm Task checks (ADR-0123). A code policy's `[warm]` table lets a check reuse one
//! machine-local, toolchain-keyed, byte-bounded build directory; a warm check changes no
//! Snapshot, candidate, check outcome or delivered tree, and only its runtime observations and
//! the `af task show` cache line say it was warm. The toolchain is a stubbed `rustc` and `cargo`
//! on the `PATH` the check receives, so every key is deterministic and nothing here builds Rust.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use review_config::lock::package_digest;
use review_source_git::Manifest;
use review_store::Cas;
use serde_json::Value;

#[path = "support/task_cli.rs"]
mod task_cli;

#[path = "support/schemas.rs"]
mod schemas;

/// The pagination check, plus one fixed-size build product in whatever `CARGO_TARGET_DIR` the
/// check received. The product is written only when absent, so a warm check finds it and a
/// cold one creates it; either way the check's own result bytes are the same.
const BUILD: &str = "import os, pagination\n\
assert pagination.paginate(list(range(7)),2,3) == [2,3,4]\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
path = os.path.join(target, 'build.bin')\n\
if not os.path.exists(path):\n    open(path, 'wb').write(b'x' * 4096)\n";

/// A check that grows its build directory past a 4096-byte bound and then keeps running, so only
/// the bound monitor can end it.
const GROW: &str = "import os, time\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
open(os.path.join(target, 'big.bin'), 'wb').write(b'x' * 8192)\n\
time.sleep(90)\n";

const TOOLCHAIN: &str = "[toolchain]\nchannel = \"1.88.0\"\n";

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    bin: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stub_toolchain(bin: &Path, rustc_version: &str) {
    for (program, text) in [
        (
            "rustc",
            format!("rustc {rustc_version} (fixture)\nhost: fixture-host-triple"),
        ),
        ("cargo", "cargo 1.88.0 (fixture)".to_string()),
    ] {
        let path = bin.join(program);
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{text}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The code policy this fixture commits: one required check running `script`, and the given
/// `[warm]` table when there is one.
fn code_policy(
    script: &str,
    check_wall_ms: u64,
    require_container: bool,
    warm: Option<&str>,
) -> String {
    let arg = |value: &str| {
        format!(
            "\n[[checks.pagination.command.args]]\nvalue = {}\nprovenance = \"literal\"\n",
            toml::Value::String(value.into())
        )
    };
    let mut text = format!(
        "schema = \"af.code-task-policy/1\"\ncheck_wall_ms = {check_wall_ms}\n\
         require_container = {require_container}\n\n[checks.pagination]\nname = \"pagination\"\n\
         required = true\n\n[checks.pagination.command]\nprogram = \"/usr/bin/python3\"\n{}{}{}",
        arg("-B"),
        arg("-c"),
        arg(script)
    );
    if let Some(warm) = warm {
        text.push_str(&format!("\n[warm]\n{warm}\n"));
    }
    text
}

fn fixture(policy: String) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let (repo, state) = task_cli::fixture_named(&root, "pagination");
    let implementer = repo.join(".af/task-packages/fixture/implementer");
    std::fs::write(
        implementer.join("worker.py"),
        "import json,sys\njson.load(sys.stdin)\n\
         open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\n\
         print(json.dumps({'schema':'af.worker-reply/1','outputs':{'report':[{'summary':'Implemented pagination'}]}}))\n",
    )
    .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"]["fixture/implementer"]["digest"] =
        toml::Value::String(package_digest("fixture/implementer", &implementer).unwrap());
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    std::fs::write(repo.join(".af/code-policy.toml"), policy).unwrap();
    std::fs::write(repo.join("rust-toolchain.toml"), TOOLCHAIN).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "warm check fixture"]);
    let fixture = Fixture {
        home: root.join("home"),
        cache: root.join("cache"),
        bin: root.join("bin"),
        root,
        repo,
        state,
        _root: directory,
    };
    for path in [
        &fixture.home,
        &fixture.cache,
        &fixture.bin,
        &fixture.root.join("tasks"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    stub_toolchain(&fixture.bin, "1.88.0");
    fixture
}

fn commit(fixture: &Fixture, path: &str, bytes: &str) {
    std::fs::write(fixture.repo.join(path), bytes).unwrap();
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-qm", "change"]);
}

fn af(fixture: &Fixture, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_CACHE_HOME", &fixture.cache)
        .env("PATH", format!("{}:/usr/bin:/bin", fixture.bin.display()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AF_CACHE_POLICY_FILE")
        .env_remove("CARGO_TARGET_DIR")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Start and execute one Task. The Task file lives outside the repository, and the ten-minute
/// wall is ADR-0114's loaded-machine budget, far above what any check here needs.
fn start(fixture: &Fixture, task_id: &str, expected: i32) -> Value {
    let file = fixture.root.join("tasks").join(format!("{task_id}.json"));
    std::fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({
            "schema": "af.task-file/1",
            "task_id": task_id,
            "kind": "implement",
            "goal": "Implement this Jira ticket: offset/limit pagination",
            "pipeline": {"name": "fixture/implementation", "fallback": "refuse"},
            "strategy": "small",
            "facts": {},
            "limits": {
                "tokens": 1000,
                "max_attempts": 3,
                "wall_ms": 600_000,
                "verification": {"tokens": 200, "attempts": 2, "wall_ms": 300_000}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, stdout, stderr) = af(
        fixture,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, expected, "{stderr}\n{stdout}");
    let outcome: Value = serde_json::from_str(stdout.trim()).unwrap();
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &outcome);
    outcome
}

fn cas(fixture: &Fixture) -> Cas {
    Cas::open_existing(fixture.state.join("cas")).unwrap()
}

fn payload(cas: &Cas, id: &str) -> Value {
    cas.get_json(id).unwrap()["payload"].clone()
}

/// Every runtime evidence record that carries cache observations: one per warm check.
fn warm_evidence(outcome: &Value) -> Vec<Value> {
    outcome["runtime_observations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|observation| observation["record"].clone())
        .filter(|record| record.get("caches").is_some())
        .collect()
}

/// The one warm check's observations, in the order they were recorded.
fn observations(outcome: &Value) -> Vec<Value> {
    let evidence = warm_evidence(outcome);
    assert_eq!(evidence.len(), 1, "one warm check: {evidence:?}");
    let spans = evidence[0]["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["kind"], "check");
    assert_eq!(spans[0]["label"], "pagination");
    evidence[0]["caches"].as_array().unwrap().clone()
}

fn check_receipt(cas: &Cas, outcome: &Value) -> Value {
    let verification = payload(
        cas,
        outcome["result"]["outputs"]["verification"]["artifact_ids"][0]
            .as_str()
            .unwrap(),
    );
    payload(cas, verification["check_receipt_id"].as_str().unwrap())
}

fn check_result(cas: &Cas, outcome: &Value) -> Value {
    let receipt = check_receipt(cas, outcome);
    cas.get_json(receipt["checks"]["pagination"].as_str().unwrap())
        .unwrap()
}

fn snapshot_manifest(cas: &Cas, snapshot_id: &str) -> Manifest {
    let snapshot = cas.get_json(snapshot_id).unwrap();
    serde_json::from_value(
        cas.get_json(snapshot["manifest_id"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap()
}

/// The payload of the candidate the implementer's Attempt returned, without its producer.
fn candidate(cas: &Cas, outcome: &Value) -> Value {
    let candidates: Vec<Value> = outcome["execution_records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["record"]["result"]["output_id"].as_str())
        .filter_map(|id| {
            let output = payload(cas, id);
            output["outputs"]["candidate"]["artifact_ids"][0]
                .as_str()
                .map(|id| payload(cas, id))
        })
        .collect();
    assert_eq!(candidates.len(), 1, "one implementer candidate");
    candidates[0].clone()
}

fn warm_directories(fixture: &Fixture) -> Vec<PathBuf> {
    let root = fixture.cache.join("af/task-build-cache");
    let mut found = Vec::new();
    for project in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for toolchain in std::fs::read_dir(project.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let directory = toolchain.path().join("cargo_target");
            if directory.exists() {
                found.push(directory);
            }
        }
    }
    found
}

fn show(fixture: &Fixture, task_id: &str) -> String {
    let (code, stdout, stderr) = af(
        fixture,
        &[
            "task",
            "show",
            task_id,
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    stdout
}

fn deliver(fixture: &Fixture, task_id: &str) -> (Value, PathBuf) {
    let worktree = fixture.root.join(format!("delivered-{task_id}"));
    let (code, stdout, stderr) = af(
        fixture,
        &[
            "task",
            "deliver",
            task_id,
            "--repo",
            fixture.repo.to_str().unwrap(),
            "--branch",
            &format!("af/{task_id}"),
            "--worktree",
            worktree.to_str().unwrap(),
            "--confirm",
            task_id,
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    (serde_json::from_str(stdout.trim()).unwrap(), worktree)
}

/// Every file below `root` except Git's own metadata, with its bytes.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn a_second_task_reuses_the_warm_directory_and_changes_no_snapshot_candidate_outcome_or_delivery() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let first = start(&fixture, "warm-one", 0);
    let second = start(&fixture, "warm-two", 0);
    let cas = cas(&fixture);

    let cold = observations(&first);
    let warm = observations(&second);
    assert_eq!(cold.len(), 1);
    assert_eq!(warm.len(), 1);
    assert_eq!(cold[0]["kind"], "cargo_target");
    assert_eq!(cold[0]["eligible"], true);
    assert_eq!(cold[0]["bytes_available"], 0, "the first check is cold");
    assert_eq!(warm[0]["kind"], "cargo_target");
    assert_eq!(warm[0]["eligible"], true);
    assert_eq!(warm[0]["bytes_available"], 4096, "the second check is warm");
    assert!(cold[0]["toolchain_id"].is_string());
    assert_eq!(warm[0]["toolchain_id"], cold[0]["toolchain_id"]);
    assert_eq!(warm[0]["source_digest"], cold[0]["source_digest"]);

    // Warmth changes nothing a check, a Snapshot or delivery produces.
    for outcome in [&first, &second] {
        assert_eq!(outcome["result"]["acceptance"], "satisfied");
    }
    assert_eq!(
        first["result"]["outputs"]["snapshot"]["snapshot_id"],
        second["result"]["outputs"]["snapshot"]["snapshot_id"]
    );
    assert_eq!(candidate(&cas, &first), candidate(&cas, &second));
    assert_eq!(
        check_receipt(&cas, &first)["checks"],
        check_receipt(&cas, &second)["checks"],
        "the same CheckResult bytes, warm or cold"
    );
    assert_eq!(check_receipt(&cas, &first)["outcome"], "passed");
    assert_eq!(check_receipt(&cas, &second)["outcome"], "passed");

    // The cache directory is private, holds the build product, and none of its bytes reached
    // a Snapshot.
    let directories = warm_directories(&fixture);
    assert_eq!(directories.len(), 1);
    assert_eq!(
        std::fs::metadata(&directories[0])
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read(directories[0].join("build.bin")).unwrap(),
        vec![b'x'; 4096]
    );
    let snapshot_id = first["result"]["outputs"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let manifest = snapshot_manifest(&cas, snapshot_id);
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.path != "build.bin"
                && !entry.path.starts_with(".af-cache")
                && !entry.path.contains("cargo_target")),
        "{:?}",
        manifest.entries
    );

    let shown = show(&fixture, "warm-one");
    assert!(shown.contains("check pagination: "), "{shown}");
    assert!(shown.contains(" ms, cargo_target cold empty"), "{shown}");
    let shown = show(&fixture, "warm-two");
    assert!(shown.contains(" ms, cargo_target warm 4096"), "{shown}");

    let (first_receipt, first_tree) = deliver(&fixture, "warm-one");
    let (second_receipt, second_tree) = deliver(&fixture, "warm-two");
    assert_eq!(tree(&first_tree), tree(&second_tree));
    assert!(
        tree(&first_tree)
            .iter()
            .all(|(path, _)| path != "build.bin")
    );
    // Everything but the per-Task identities, branch and worktree is the same delivery.
    for key in [
        "source_snapshot_id",
        "derived_snapshot_id",
        "outcome",
        "ignored_paths",
        "undeclared_af_paths",
        "remote_actions",
    ] {
        assert_eq!(first_receipt[key], second_receipt[key], "{key}");
    }
    assert_eq!(
        first_receipt["target"]["repository_id"],
        second_receipt["target"]["repository_id"]
    );
}

#[test]
fn a_changed_toolchain_declaration_or_compiler_is_a_new_key_and_a_cold_check() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    start(&fixture, "key-one", 0);
    let reused = observations(&start(&fixture, "key-two", 0));
    assert_eq!(reused[0]["bytes_available"], 4096);
    let first_key = reused[0]["toolchain_id"].clone();

    commit(
        &fixture,
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"1.89.0\"\n",
    );
    let declared = observations(&start(&fixture, "key-three", 0));
    assert_eq!(declared[0]["kind"], "cargo_target");
    assert_eq!(declared[0]["eligible"], true);
    assert_eq!(declared[0]["bytes_available"], 0, "a new key starts cold");
    assert_ne!(declared[0]["toolchain_id"], first_key);

    stub_toolchain(&fixture.bin, "1.90.0");
    let compiled = observations(&start(&fixture, "key-four", 0));
    assert_eq!(
        compiled[0]["bytes_available"], 0,
        "a new compiler starts cold"
    );
    assert_ne!(compiled[0]["toolchain_id"], first_key);
    assert_ne!(compiled[0]["toolchain_id"], declared[0]["toolchain_id"]);
    assert_eq!(warm_directories(&fixture).len(), 3);
}

#[test]
fn a_directory_above_its_bound_is_removed_before_the_check_which_runs_cold() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    start(&fixture, "bound-one", 0);
    assert_eq!(warm_directories(&fixture).len(), 1);
    commit(
        &fixture,
        ".af/code-policy.toml",
        &code_policy(
            BUILD,
            60_000,
            false,
            Some("build_cache = [\"cargo_target\"]\nmax_bytes = 1"),
        ),
    );
    let outcome = start(&fixture, "bound-two", 0);
    let observed = observations(&outcome);
    assert_eq!(observed.len(), 1, "{observed:?}");
    assert_eq!(observed[0]["kind"], "cargo_target:bound_exceeded");
    assert_eq!(observed[0]["eligible"], false);
    assert_eq!(observed[0]["bytes_available"], 4096);
    assert!(
        warm_directories(&fixture).is_empty(),
        "removed, and the cold check built in its private runtime directory"
    );
    assert_eq!(outcome["result"]["acceptance"], "satisfied");
    let shown = show(&fixture, "bound-two");
    assert!(
        shown.contains("cargo_target cold bound_exceeded"),
        "{shown}"
    );
}

#[test]
fn a_check_that_writes_past_the_bound_is_ended_failed_and_its_directory_removed() {
    let fixture = fixture(code_policy(
        GROW,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096"),
    ));
    let started = std::time::Instant::now();
    let outcome = start(&fixture, "bound-grow", 3);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(80),
        "the monitor ended the check before it finished sleeping"
    );
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_bound_exceeded");
    assert_eq!(check_receipt(&cas, &outcome)["outcome"], "failed");
    assert_eq!(outcome["result"]["acceptance"], "unsatisfied");
    let observed = observations(&outcome);
    assert_eq!(
        observed.len(),
        1,
        "one declared kind, one observation: {observed:?}"
    );
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert_eq!(observed[0]["eligible"], true);
    assert_eq!(observed[0]["bytes_available"], 0);
    assert_eq!(observed[0]["evicted_bytes"], 8192);
    assert!(
        warm_directories(&fixture).is_empty(),
        "the directory is gone"
    );
    let shown = show(&fixture, "bound-grow");
    assert!(
        shown.contains("cargo_target cold empty, removed 8192 (bound_exceeded)"),
        "{shown}"
    );
}

#[test]
fn warm_with_require_container_is_refused_before_any_attempt() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        true,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let file = fixture.root.join("tasks/refused.json");
    std::fs::write(
        &file,
        std::fs::read_to_string(fixture.repo.join("ticket.json"))
            .unwrap()
            .replace("pagination-cli", "refused"),
    )
    .unwrap();
    let (code, stdout, stderr) = af(
        &fixture,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_ne!(code, 0, "{stdout}");
    assert!(stderr.contains("[warm]"), "{stderr}");
    assert!(stderr.contains("require_container = true"), "{stderr}");
    let (_, listed, _) = af(
        &fixture,
        &[
            "task",
            "list",
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!listed.contains("refused"), "{listed}");
    assert!(warm_directories(&fixture).is_empty());
    assert!(!fixture.cache.join("af/task-build-cache").exists());
}

#[test]
fn a_policy_without_warm_captures_and_records_exactly_what_it_did_before() {
    // The committed pagination policy, untouched: no `[warm]`.
    let fixture = fixture(
        std::fs::read_to_string(
            std::env::var_os("AF_WORKSPACE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
                .join("fixtures/task-runtime/pagination/.af/code-policy.toml"),
        )
        .unwrap(),
    );
    let outcome = start(&fixture, "cold-only", 0);
    let cas = cas(&fixture);

    // The captured policy is the committed TOML read as plain data: no field the new table
    // could add, so the revision and plan that name it keep their identities.
    let committed: Value = toml::from_str::<toml::Value>(
        &std::fs::read_to_string(fixture.repo.join(".af/code-policy.toml")).unwrap(),
    )
    .map(|value| serde_json::to_value(value).unwrap())
    .unwrap();
    let policy_id = check_receipt(&cas, &outcome)["policy_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(cas.get_json(&policy_id).unwrap(), committed);
    assert_eq!(cas.put_json(&committed).unwrap(), policy_id);

    // One runtime record per check Attempt, with its span and no cache observation.
    assert!(warm_evidence(&outcome).is_empty());
    let checks: Vec<&Value> = outcome["runtime_observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|observation| {
            observation["record"]["spans"]
                .as_array()
                .is_some_and(|spans| spans.iter().any(|span| span["kind"] == "check"))
        })
        .collect();
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0]["record"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["task_id", "attempt_id", "node", "context_id", "spans"]
    );
    let shown = show(&fixture, "cold-only");
    assert!(!shown.contains("check "), "{shown}");
    assert!(!fixture.cache.join("af/task-build-cache").exists());
}
