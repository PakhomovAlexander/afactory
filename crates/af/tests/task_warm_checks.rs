//! Warm Task checks (ADR-0123). A code policy's `[warm]` table lets a check reuse one
//! machine-local, toolchain-keyed, byte-bounded build directory; a warm check changes no
//! Snapshot, candidate, check outcome or delivered tree, and only its runtime observations and
//! the `af task show` cache line say it was warm. The toolchain is a stubbed `rustc` and `cargo`
//! on the `PATH` the check receives, so every key is deterministic and nothing here builds Rust.
//! Like a rustup proxy, the stub answers only when it received the kernel's `RUSTUP_HOME` and
//! `RUSTUP_AUTO_INSTALL=0`, so a warm key here proves no toolchain download was needed. A Task
//! without `[warm]` is compared with the documents a kernel without the warm package recorded.

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

/// A check that deletes its own warm directory and exits successfully.
const REMOVE_ROOT: &str = "import os, shutil\n\
shutil.rmtree(os.environ['CARGO_TARGET_DIR'])\n";

/// A check that unlinks the kernel's held key lock and parks an oversized file at its name.
const REPLACE_LOCK: &str = "import os\n\
key = os.path.dirname(os.environ['CARGO_TARGET_DIR'])\n\
os.remove(os.path.join(key, 'warm.lock'))\n\
open(os.path.join(key, 'warm.lock'), 'wb').write(b'x' * 8192)\n";

/// A check that widens the toolchain key directory and exits successfully.
const WIDEN_KEY: &str = "import os\n\
os.chmod(os.path.dirname(os.environ['CARGO_TARGET_DIR']), 0o777)\n";

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

/// A stubbed rustup proxy: like the real one it answers only from an installed toolchain, so
/// without `RUSTUP_HOME`, or with auto-install left on, it fails as a download would instead of
/// answering. A warm check that never received the kernel's rustup home therefore cannot key.
fn stub_toolchain(bin: &Path, rustc_version: &str) {
    for (program, text) in [
        (
            "rustc",
            format!("rustc {rustc_version} (fixture)\nhost: fixture-host-triple"),
        ),
        ("cargo", "cargo 1.88.0 (fixture)".to_string()),
    ] {
        let path = bin.join(program);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 if [ -z \"$RUSTUP_HOME\" ] || [ \"$RUSTUP_AUTO_INSTALL\" != 0 ]; then\n\
                 echo 'info: syncing channel updates: would download a toolchain' >&2\n\
                 exit 1\n\
                 fi\n\
                 printf '%s\\n' '{text}'\n"
            ),
        )
        .unwrap();
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
    // The kernel's installed toolchain lives in its own HOME's rustup home.
    for path in [
        &fixture.home.join(".rustup"),
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
    af_with(fixture, &[], args)
}

/// `af` under the fixture's kernel environment plus `extra`.
fn af_with(fixture: &Fixture, extra: &[(&str, &Path)], args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_CACHE_HOME", &fixture.cache)
        .env("PATH", format!("{}:/usr/bin:/bin", fixture.bin.display()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AF_CACHE_POLICY_FILE")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_HOME")
        .env_remove("RUSTUP_HOME")
        .env_remove("RUSTUP_TOOLCHAIN")
        .envs(extra.iter().copied())
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
    start_with(fixture, &[], task_id, expected)
}

fn start_with(fixture: &Fixture, extra: &[(&str, &Path)], task_id: &str, expected: i32) -> Value {
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
    let (code, stdout, stderr) = af_with(
        fixture,
        extra,
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
    warm_directories_of(fixture, "cargo_target")
}

fn warm_directories_of(fixture: &Fixture, kind: &str) -> Vec<PathBuf> {
    let root = fixture.cache.join("af/task-build-cache");
    let mut found = Vec::new();
    for project in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        for toolchain in std::fs::read_dir(project.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let directory = toolchain.path().join(kind);
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
fn a_check_that_removes_its_warm_root_fails_as_suspect_with_the_cause_recorded() {
    let fixture = fixture(code_policy(
        REMOVE_ROOT,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let outcome = start(&fixture, "remove-root", 3);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_suspect");
    assert_eq!(check_receipt(&cas, &outcome)["outcome"], "failed");
    let observed = observations(&outcome);
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert_eq!(
        observed[0]["evicted_bytes"], 0,
        "nothing was left to remove"
    );
    assert_eq!(observed[0]["evicted_reason"], "suspect");
    assert!(warm_directories(&fixture).is_empty());
    let shown = show(&fixture, "remove-root");
    assert!(shown.contains("check pagination: failed in "), "{shown}");
    assert!(
        shown.contains("cargo_target cold empty, removed 0 (suspect)"),
        "{shown}"
    );
}

#[test]
fn a_check_that_replaces_the_key_lock_with_an_oversized_file_fails_and_the_key_is_emptied() {
    let fixture = fixture(code_policy(
        REPLACE_LOCK,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096"),
    ));
    let outcome = start(&fixture, "replace-lock", 3);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(check_receipt(&cas, &outcome)["outcome"], "failed");
    assert!(
        warm_directories(&fixture).is_empty(),
        "every kind below the key went"
    );
    let observed = observations(&outcome);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert!(observed[0]["evicted_reason"].is_string(), "{observed:?}");
}

#[test]
fn a_check_that_widens_the_toolchain_key_fails_as_suspect_and_the_key_is_emptied() {
    let fixture = fixture(code_policy(
        WIDEN_KEY,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let outcome = start(&fixture, "widen-key", 3);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_suspect");
    assert!(warm_directories(&fixture).is_empty());
    let shown = show(&fixture, "widen-key");
    assert!(shown.contains("removed 0 (suspect)"), "{shown}");
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

/// The pagination check under both warm kinds. It requires the kernel's rustup home and no
/// auto-install, as a real rustup proxy would, and writes one fixed-size product into each
/// directory it received, only when absent.
const BUILD_BOTH: &str = "import os, pagination\n\
assert pagination.paginate(list(range(7)),2,3) == [2,3,4]\n\
assert os.environ['RUSTUP_AUTO_INSTALL'] == '0'\n\
assert os.path.isdir(os.environ['RUSTUP_HOME'])\n\
home = os.environ['CARGO_HOME']\n\
assert not home.startswith(os.environ['HOME'])\n\
def once(directory, name, size):\n    \
os.makedirs(directory, exist_ok=True)\n    \
path = os.path.join(directory, name)\n    \
os.path.exists(path) or open(path, 'wb').write(b'x' * size)\n\
once(os.environ['CARGO_TARGET_DIR'], 'build.bin', 4096)\n\
once(os.path.join(home, 'registry'), 'index.bin', 1024)\n";

/// Past a 4096-byte bound and done at once: it ends long before the monitor's first sample.
const FAST_GROW: &str = "import os\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
open(os.path.join(target, 'big.bin'), 'wb').write(b'x' * 8192)\n";

/// Hides 2 MiB behind a directory nobody can read, then passes.
const HIDE: &str = "import os\n\
hidden = os.path.join(os.environ['CARGO_TARGET_DIR'], 'hidden')\n\
os.makedirs(hidden, exist_ok=True)\n\
open(os.path.join(hidden, 'big.bin'), 'wb').write(b'x' * 2097152)\n\
os.chmod(hidden, 0)\n";

const BOTH: &str = "build_cache = [\"cargo_target\", \"cargo_home\"]";

/// Mode 000 binds only an unprivileged kernel; as root the unreadable fixtures cannot exist.
fn permissions_bind() -> bool {
    let probe = tempfile::tempdir().unwrap();
    std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(probe.path()).is_ok();
    std::fs::set_permissions(probe.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    !readable
}

/// Every warm evidence group, each naming its check.
fn named_groups(outcome: &Value) -> Vec<Value> {
    let groups = warm_evidence(outcome);
    for group in &groups {
        assert!(group["check"]["name"].is_string(), "unnamed group: {group}");
        for span in group["spans"].as_array().into_iter().flatten() {
            assert_eq!(span["label"], group["check"]["name"]);
        }
    }
    groups
}

fn kinds(group: &Value) -> Vec<(String, bool, u64)> {
    group["caches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cache| {
            (
                cache["kind"].as_str().unwrap().to_owned(),
                cache["eligible"].as_bool().unwrap(),
                cache["bytes_available"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn both_kinds_are_warm_on_the_second_run_from_the_installed_toolchain() {
    let fixture = fixture(code_policy(BUILD_BOTH, 60_000, false, Some(BOTH)));
    let first = start(&fixture, "home-one", 0);
    let second = start(&fixture, "home-two", 0);

    let cold = named_groups(&first);
    let warm = named_groups(&second);
    assert_eq!(cold.len(), 1);
    assert_eq!(warm.len(), 1);
    assert_eq!(
        kinds(&cold[0]),
        [
            ("cargo_target".into(), true, 0),
            ("cargo_home".into(), true, 0)
        ]
    );
    assert_eq!(
        kinds(&warm[0]),
        [
            ("cargo_target".into(), true, 4096),
            ("cargo_home".into(), true, 1024)
        ],
        "the second run is warm for both kinds"
    );
    assert_eq!(
        warm[0]["check"],
        serde_json::json!({"name": "pagination", "outcome": "passed", "rustup_home": "kernel_home"})
    );
    let caches = warm[0]["caches"].as_array().unwrap();
    assert_eq!(caches[0]["toolchain_id"], caches[1]["toolchain_id"]);
    assert_eq!(
        caches[0]["toolchain_id"],
        cold[0]["caches"][0]["toolchain_id"]
    );
    let homes = warm_directories_of(&fixture, "cargo_home");
    assert_eq!(homes.len(), 1);
    assert_eq!(
        homes[0].parent(),
        warm_directories(&fixture)[0].parent(),
        "one toolchain key holds both kinds"
    );
    assert_eq!(
        std::fs::read(homes[0].join("registry/index.bin")).unwrap(),
        vec![b'x'; 1024]
    );
    assert!(
        !fixture
            .home
            .join(".rustup")
            .read_dir()
            .unwrap()
            .any(|_| true),
        "the rustup home is read, never written"
    );
    let shown = show(&fixture, "home-two");
    assert!(shown.contains("check pagination: passed in "), "{shown}");
    assert!(
        shown.contains(" ms, cargo_target warm 4096, cargo_home warm 1024"),
        "{shown}"
    );

    // The kernel's own RUSTUP_HOME wins over its HOME's, and names another toolchain key.
    let elsewhere = fixture.root.join("rustup-elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let explicit = start_with(&fixture, &[("RUSTUP_HOME", &elsewhere)], "home-three", 0);
    let group = &named_groups(&explicit)[0];
    assert_eq!(group["check"]["rustup_home"], "kernel_environment");
    assert_ne!(
        group["caches"][0]["toolchain_id"], caches[0]["toolchain_id"],
        "the rustup home is part of the toolchain key"
    );
    assert_eq!(kinds(group)[0], ("cargo_target".into(), true, 0));
}

#[test]
fn without_a_rustup_home_the_probe_fails_and_the_check_runs_cold_with_its_reason() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    std::fs::remove_dir(fixture.home.join(".rustup")).unwrap();
    let outcome = start(&fixture, "no-rustup", 0);
    let groups = named_groups(&outcome);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        kinds(&groups[0]),
        [("cargo_target:toolchain_unresolved".into(), false, 0)]
    );
    assert_eq!(groups[0]["check"]["rustup_home"], "unset_not_installed");
    assert_eq!(groups[0]["check"]["outcome"], "passed");
    assert!(warm_directories(&fixture).is_empty());
    let shown = show(&fixture, "no-rustup");
    assert!(
        shown.contains("cargo_target cold toolchain_unresolved"),
        "{shown}"
    );
}

#[test]
fn a_fast_check_that_writes_past_the_bound_fails_and_its_directory_is_removed() {
    let fixture = fixture(code_policy(
        FAST_GROW,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096"),
    ));
    let outcome = start(&fixture, "fast-grow", 3);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_bound_exceeded");
    assert_eq!(check_receipt(&cas, &outcome)["outcome"], "failed");
    let groups = named_groups(&outcome);
    assert_eq!(groups[0]["check"]["outcome"], "failed");
    assert!(
        groups[0]["spans"][0]["elapsed_ms"].as_u64().unwrap() < 5_000,
        "the command ended before the monitor's first sample"
    );
    let observed = observations(&outcome);
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert_eq!(observed[0]["evicted_bytes"], 8192);
    assert!(warm_directories(&fixture).is_empty());
    let shown = show(&fixture, "fast-grow");
    assert!(shown.contains("check pagination: failed in "), "{shown}");
    assert!(
        shown.contains("cargo_target cold empty, removed 8192 (bound_exceeded)"),
        "{shown}"
    );
}

#[test]
fn an_unreadable_subtree_a_check_leaves_fails_it_through_the_bound() {
    if !permissions_bind() {
        return;
    }
    let fixture = fixture(code_policy(
        HIDE,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 1048576"),
    ));
    let outcome = start(&fixture, "hidden-grow", 3);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_bound_exceeded");
    let observed = observations(&outcome);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert!(observed[0]["evicted_bytes"].is_u64());
    assert!(
        warm_directories(&fixture).is_empty(),
        "removed under the lock, unreadable child and all"
    );
}

/// One way to make a populated warm directory suspect.
type Plant = Box<dyn Fn(&Path)>;

#[test]
fn a_discarded_suspect_directory_is_a_cold_check_never_its_old_bytes() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 1048576"),
    ));
    start(&fixture, "suspect-populate", 0);
    let directory = warm_directories(&fixture).remove(0);
    assert_eq!(
        observations(&start(&fixture, "suspect-warm", 0))[0]["bytes_available"],
        4096
    );

    let mut planted: Vec<(&str, Plant)> = vec![
        (
            "widened",
            Box::new(|directory: &Path| {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }),
        ),
        (
            "linked",
            Box::new(|directory: &Path| {
                std::os::unix::fs::symlink("/etc/hosts", directory.join("link")).unwrap();
            }),
        ),
    ];
    if permissions_bind() {
        // An unreadable child holding more than the bound: a byte walk that skipped it would
        // call the directory small and reuse it.
        planted.push((
            "unreadable",
            Box::new(|directory: &Path| {
                let hidden = directory.join("hidden");
                std::fs::create_dir(&hidden).unwrap();
                std::fs::write(hidden.join("big.bin"), vec![0_u8; 2 * 1024 * 1024]).unwrap();
                std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).unwrap();
            }),
        ));
    }
    for (label, plant) in planted {
        assert!(
            directory.join("build.bin").exists(),
            "{label}: populated before"
        );
        plant(&directory);
        let task_id = format!("suspect-{label}");
        let outcome = start(&fixture, &task_id, 0);
        let observed = observations(&outcome);
        let shown = show(&fixture, &task_id);
        if label == "unreadable" {
            // What the kernel cannot count it cannot bound: the whole key is uninspectable
            // before the check, so every kind is evicted and the check runs cold with that
            // reason, never with the directory's old bytes.
            assert_eq!(
                observed[0]["kind"], "cargo_target:bound_exceeded",
                "{label}"
            );
            assert_eq!(observed[0]["eligible"], false, "{label}");
            assert!(
                shown.contains("cargo_target cold bound_exceeded"),
                "{label}: {shown}"
            );
        } else {
            assert_eq!(observed[0]["kind"], "cargo_target", "{label}");
            assert_eq!(observed[0]["eligible"], true, "{label}");
            assert_eq!(
                observed[0]["bytes_available"], 0,
                "{label}: recreated, so a cold check"
            );
            assert!(
                shown.contains("cargo_target cold empty"),
                "{label}: {shown}"
            );
        }
        assert!(!shown.contains("cargo_target warm"), "{label}: {shown}");
        assert!(!directory.join("link").exists() && !directory.join("hidden").exists());
        if label == "unreadable" {
            // Evicted with the whole key: the check built cold in its private runtime
            // directory, so nothing sits at the kind's name until the next warm check.
            assert!(
                std::fs::symlink_metadata(&directory).is_err(),
                "{label}: evicted"
            );
        } else {
            let metadata = std::fs::symlink_metadata(&directory).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700, "{label}");
        }
    }
}

/// Make the fixture Pipeline's check node run `names`, re-pinning its package digest.
fn check_node_runs(fixture: &Fixture, names: &[&str]) {
    let package = fixture
        .repo
        .join(".af/task-packages/fixture/implementation");
    let pipeline = package.join("pipeline.toml");
    let text = std::fs::read_to_string(&pipeline).unwrap();
    let declared = "checks = [\"pagination\"]";
    assert!(text.contains(declared));
    std::fs::write(
        &pipeline,
        text.replace(declared, &format!("checks = {names:?}")),
    )
    .unwrap();
    let catalog_path = fixture.repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"]["fixture/implementation"]["digest"] =
        toml::Value::String(package_digest("fixture/implementation", &package).unwrap());
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-qm", "check both"]);
}

/// `script` as one more required check named `name`, appended to `policy`.
fn with_check(policy: String, name: &str, script: &str) -> String {
    format!(
        "{policy}\n[checks.{name}]\nname = \"{name}\"\nrequired = true\n\n\
         [checks.{name}.command]\nprogram = \"/usr/bin/python3\"\n\n\
         [[checks.{name}.command.args]]\nvalue = \"-c\"\nprovenance = \"literal\"\n\n\
         [[checks.{name}.command.args]]\nvalue = {}\nprovenance = \"literal\"\n",
        toml::Value::String(script.into())
    )
}

#[test]
fn a_check_refused_its_declared_cache_keeps_its_name_and_every_kind() {
    let fixture = fixture(code_policy(
        BUILD_BOTH,
        60_000,
        false,
        Some(&format!("{BOTH}\ncaches = [\"cargo\"]")),
    ));
    let outcome = start(&fixture, "refused-cache", 4);
    let groups = named_groups(&outcome);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]["check"],
        serde_json::json!({"name": "pagination", "outcome": "not_run", "rustup_home": "kernel_home"})
    );
    assert!(groups[0].get("spans").is_none());
    assert_eq!(
        kinds(&groups[0]),
        [
            ("cargo_target:cache_refused".into(), false, 0),
            ("cargo_home:superseded".into(), false, 0),
            ("cargo:unavailable".into(), false, 0),
        ]
    );
    let shown = show(&fixture, "refused-cache");
    assert!(
        shown.contains(
            "check pagination: not_run, never started, cargo_target cold cache_refused, \
             cargo_home cold superseded, cargo cold unavailable"
        ),
        "{shown}"
    );
    assert!(!shown.contains("check not run"), "{shown}");
}

#[test]
fn every_check_of_a_warm_task_has_one_named_group_even_when_the_deadline_skips_it() {
    // `exhaust` sorts first and sleeps past the check Attempt's wall, so `pagination` finds no
    // time left before its preparation.
    let fixture = fixture(with_check(
        code_policy(BUILD_BOTH, 5_000, false, Some(BOTH)),
        "exhaust",
        "import time\ntime.sleep(60)\n",
    ));
    check_node_runs(&fixture, &["exhaust", "pagination"]);
    let outcome = start(&fixture, "deadline-skip", 4);
    let groups = named_groups(&outcome);
    let names: Vec<_> = groups
        .iter()
        .map(|group| group["check"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["exhaust", "pagination"], "one group per check");
    for group in &groups {
        assert_eq!(group["caches"].as_array().unwrap().len(), 2, "{group}");
    }
    // How far `exhaust` got inside its wall depends on the machine's load; that it ended
    // without passing does not.
    assert_ne!(groups[0]["check"]["outcome"], "passed");
    assert_eq!(groups[1]["check"]["outcome"], "not_run");
    assert!(groups[1].get("spans").is_none());
    assert_eq!(
        kinds(&groups[1]),
        [
            ("cargo_target:deadline_exhausted".into(), false, 0),
            ("cargo_home:deadline_exhausted".into(), false, 0),
        ]
    );
    let shown = show(&fixture, "deadline-skip");
    assert!(shown.contains("check exhaust: "), "{shown}");
    assert!(
        shown.contains(
            "check pagination: not_run, never started, cargo_target cold deadline_exhausted, \
             cargo_home cold deadline_exhausted"
        ),
        "{shown}"
    );
    assert!(!shown.contains("check not run"), "{shown}");
}

/// The checked-in documents a kernel without the Warm Check Cache package recorded for the
/// Task below: its revision, plan, `af task show --json` inspection and delivery receipt,
/// normalized by [`run_independent`].
const NO_WARM_DOCUMENTS: &str = include_str!("fixtures/task-warm/no-warm-documents.json");

/// Pair two independent runs' documents. A value equal in both is kept literally: the captured
/// policy, the Snapshots, the candidate and the check result bytes are content identities that
/// do not depend on when or where the Task ran. A value that differs between the runs — a
/// deadline and every identity derived from it, an Attempt ULID, a writer pid, a fixture path —
/// becomes `<run:N>`, N its first appearance, and the two runs must pair such values one to
/// one, so every equality between them survives. Host timings are masked first, because two
/// runs can agree on a duration by chance.
#[derive(Default)]
struct RunIndependent {
    first: std::collections::BTreeMap<String, usize>,
    second: std::collections::BTreeMap<String, usize>,
}

impl RunIndependent {
    fn pair(&mut self, first: &Value, second: &Value, key: &str) -> Value {
        match (first, second) {
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(
                    a.keys().collect::<Vec<_>>(),
                    b.keys().collect::<Vec<_>>(),
                    "two runs record the same fields at {key}"
                );
                Value::Object(
                    a.iter()
                        .map(|(name, value)| (name.clone(), self.pair(value, &b[name], name)))
                        .collect(),
                )
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "two runs record as many entries at {key}");
                Value::Array(a.iter().zip(b).map(|(a, b)| self.pair(a, b, key)).collect())
            }
            _ if key.ends_with("_unix_ms") || key == "elapsed_ms" => Value::String("<ms>".into()),
            _ if first == second => first.clone(),
            _ => {
                let next = self.first.len();
                let a = *self.first.entry(first.to_string()).or_insert(next);
                let b = *self.second.entry(second.to_string()).or_insert(next);
                assert_eq!(a, b, "run-dependent values pair one to one at {key}");
                Value::String(format!("<run:{a}>"))
            }
        }
    }
}

fn run_independent(first: &Value, second: &Value) -> Value {
    RunIndependent::default().pair(first, second, "")
}

/// The revision, plan, inspection and delivery documents of one Task without `[warm]`, run in
/// a fixture whose Git history is pinned, so its Snapshots are the same in every run.
fn no_warm_documents() -> Value {
    let fixture = pinned_fixture();
    start(&fixture, "cold-only", 0);
    let (code, shown, stderr) = af(
        &fixture,
        &[
            "task",
            "show",
            "cold-only",
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let inspection: Value = serde_json::from_str(shown.trim()).unwrap();
    let (delivery, worktree) = deliver(&fixture, "cold-only");
    let cas = cas(&fixture);
    let document = |id: &Value| cas.get_json(id.as_str().unwrap()).unwrap();
    assert!(
        !fixture.cache.join("af/task-build-cache").exists(),
        "no warm directory exists"
    );
    let plan = document(&inspection["plan_id"]);
    let authority_id = plan["payload"]["authority"]["policy_id"].clone();
    let authority = cas.get_json(authority_id.as_str().unwrap()).unwrap();
    let engine_id = plan["payload"]["engine_id"].clone();
    assert!(engine_id.is_string());
    assert_eq!(
        authority["engine_id"], engine_id,
        "the run authority binds the running engine"
    );
    let documents = serde_json::json!({
        "check_receipt": check_receipt(&cas, &inspection),
        "check_result": check_result(&cas, &inspection),
        "authority": authority,
        "revision": document(&inspection["revision_id"]),
        "plan": plan,
        "inspection": inspection,
        "delivery": delivery,
        "delivered_tree": tree(&worktree)
            .into_iter()
            .map(|(path, bytes)| (path, review_store::canonical::blob_content_id(&bytes)))
            .collect::<std::collections::BTreeMap<_, _>>(),
    });
    // The engine is the digest of the running `af` executable's bytes, so every build has its
    // own, and the run authority is the one document that hashes it. Both are masked; every
    // other field of the authority, the code policy included, is compared as recorded.
    without_engine(documents, &engine_id, &authority_id)
}

fn without_engine(value: Value, engine_id: &Value, authority_id: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, without_engine(value, engine_id, authority_id)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|value| without_engine(value, engine_id, authority_id))
                .collect(),
        ),
        value if value == *engine_id => Value::String("<engine>".into()),
        value if value == *authority_id => Value::String("<authority of the engine>".into()),
        value => value,
    }
}

#[test]
fn a_task_without_warm_records_the_documents_a_kernel_without_warm_recorded() {
    let normalized = run_independent(&no_warm_documents(), &no_warm_documents());
    let expected: Value = serde_json::from_str(NO_WARM_DOCUMENTS).unwrap();
    assert!(
        normalized == expected,
        "a Task without [warm] changed a document:\n{}",
        serde_json::to_string_pretty(&normalized).unwrap()
    );
    // The literal identities the comparison binds, not only its shape.
    let literal = |value: &Value| {
        value
            .as_str()
            .is_some_and(|text| text.starts_with("sha256:"))
    };
    for path in [
        "/revision/payload/acceptance/verified/verifier_policy",
        "/revision/payload/inputs/source/snapshot_id",
        "/delivery/source_snapshot_id",
        "/delivery/derived_snapshot_id",
    ] {
        assert!(literal(expected.pointer(path).unwrap()), "{path}");
    }
}

#[test]
fn no_warm_cache_byte_reaches_a_worker_sandbox_a_snapshot_or_a_delivered_tree() {
    let fixture = fixture(code_policy(BUILD_BOTH, 60_000, false, Some(BOTH)));
    let outcomes = [
        start(&fixture, "sandbox-one", 0),
        start(&fixture, "sandbox-two", 0),
    ];
    assert_eq!(
        kinds(&named_groups(&outcomes[1])[0]),
        [
            ("cargo_target".into(), true, 4096),
            ("cargo_home".into(), true, 1024)
        ],
        "the second Task ran warm, so its Worker ran beside populated directories"
    );

    // Every non-empty file of both warm directories, by content identity.
    let mut cached = std::collections::BTreeSet::new();
    let mut directories = warm_directories_of(&fixture, "cargo_target");
    directories.extend(warm_directories_of(&fixture, "cargo_home"));
    assert_eq!(directories.len(), 2);
    for directory in &directories {
        for (_, bytes) in tree(directory) {
            if !bytes.is_empty() {
                cached.insert(review_store::canonical::blob_content_id(&bytes));
            }
        }
    }
    assert_eq!(cached.len(), 2, "build.bin and index.bin");
    let names = [
        "cargo_target",
        "cargo_home",
        "task-build-cache",
        ".af-cache",
        "build.bin",
        "index.bin",
    ];
    let clean = |label: &str, entries: Vec<(String, String)>| {
        assert!(!entries.is_empty(), "{label} is not empty");
        for (path, content) in entries {
            assert!(
                !names.iter().any(|name| path.contains(name)),
                "{label} holds a cache path: {path}"
            );
            assert!(
                !cached.contains(&content),
                "{label} holds cache bytes at {path}"
            );
        }
    };
    let manifest_entries = |manifest: Manifest| {
        manifest
            .entries
            .into_iter()
            .map(|entry| (entry.path.clone(), entry.content.clone()))
            .collect::<Vec<_>>()
    };

    let cas = cas(&fixture);
    for (index, outcome) in outcomes.iter().enumerate() {
        // The implementer Worker's sandbox is materialized from the exact source Snapshot its
        // Attempt context bound.
        let contexts: Vec<Value> = outcome["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["record"]["context_id"].as_str())
            .map(|id| payload(&cas, id))
            .filter(|context| context["invocation"]["node"] == "root.nodes.implement")
            .collect();
        assert_eq!(contexts.len(), 1, "one implementer Attempt");
        let sandbox = contexts[0]["invocation"]["inputs"]["source"]["snapshot_id"]
            .as_str()
            .unwrap();
        clean(
            &format!("run {index}: the implementer's sandbox manifest"),
            manifest_entries(snapshot_manifest(&cas, sandbox)),
        );
        let sealed = candidate(&cas, outcome);
        let sealed: Manifest = serde_json::from_value(
            cas.get_json(sealed["manifest_id"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        clean(
            &format!("run {index}: the sealed candidate manifest"),
            manifest_entries(sealed),
        );
        let derived = outcome["result"]["outputs"]["snapshot"]["snapshot_id"]
            .as_str()
            .unwrap();
        clean(
            &format!("run {index}: the derived Snapshot"),
            manifest_entries(snapshot_manifest(&cas, derived)),
        );
        let (_, worktree) = deliver(&fixture, outcome["task_id"].as_str().unwrap());
        clean(
            &format!("run {index}: the delivered tree"),
            tree(&worktree)
                .into_iter()
                .map(|(path, bytes)| (path, review_store::canonical::blob_content_id(&bytes)))
                .collect(),
        );
    }
    // Nothing the cache holds is a file in the CAS's Snapshot content either way round: the
    // directory was never captured.
    for content in &cached {
        assert!(
            cas.get(content).is_err(),
            "cache bytes {content} were stored in the CAS"
        );
    }
}

fn pinned_fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = root.join("repo");
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    copy(&workspace.join("fixtures/task-runtime/pagination"), &repo);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &["commit", "-qm", "fixture"],
    ] {
        pinned_git(&repo, args);
    }
    let fixture = Fixture {
        home: root.join("home"),
        cache: root.join("cache"),
        bin: root.join("bin"),
        state: root.join("state"),
        root,
        repo,
        _root: directory,
    };
    for path in [
        &fixture.home.join(".rustup"),
        &fixture.cache,
        &fixture.bin,
        &fixture.root.join("tasks"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    stub_toolchain(&fixture.bin, "1.88.0");
    fixture
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn pinned_git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
