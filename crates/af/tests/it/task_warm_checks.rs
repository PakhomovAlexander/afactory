//! Warm Task checks (ADR-0131). A code policy's `[warm]` table lets a check reuse one
//! machine-local, toolchain-keyed, byte-bounded build directory; a warm check changes no
//! Snapshot, candidate, check outcome or delivered tree, and only its runtime observations and
//! the `af task show` cache line say it was warm. The toolchain is a stubbed `rustc` and `cargo`
//! on the `PATH` the check receives, so every key is deterministic and nothing here builds Rust.
//! Like a rustup proxy, the stub answers only when it received the kernel's `RUSTUP_HOME` and
//! `RUSTUP_AUTO_INSTALL=0`, so a warm key here proves no toolchain download was needed. A Task
//! without `[warm]` is compared with the documents a kernel without the warm package recorded.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use review_config::lock::package_digest;
use review_source_git::Manifest;
use review_store::Cas;
use serde_json::Value;

use crate::task_cli;

use crate::schemas;

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

/// A check that takes write permission away from the toolchain key and exits successfully.
const READ_ONLY_KEY: &str = "import os\n\
os.chmod(os.path.dirname(os.environ['CARGO_TARGET_DIR']), 0o500)\n";

/// A check that parks an oversized file with a non-UTF-8 name directly below the key.
const ODD_NAME: &str = "import os\n\
key = os.path.dirname(os.environ['CARGO_TARGET_DIR']).encode()\n\
open(os.path.join(key, b'\\xff\\xfe-junk'), 'wb').write(b'x' * 8192)\n";

/// A check that finds every regular file of its private tree stamped
/// with one modification time (ADR-0161). The stamp is later than its private `HOME`, which the
/// kernel creates after the tree is materialized and before the check holds the warm key, and
/// later than the build product the previous warm check left; it leaves its own product behind.
const STAMPED: &str = "import os, stat\n\
stamps = set()\n\
paths = [os.path.join(p, n) for p, ds, fs in os.walk('.') for n in ds + fs]\n\
entries = [os.lstat(path) for path in paths]\n\
stamps |= {e.st_mtime_ns for e in entries if stat.S_ISREG(e.st_mode)}\n\
assert len(stamps) == 1, sorted(stamps)\n\
stamp = stamps.pop()\n\
assert stamp > os.lstat(os.environ['HOME']).st_mtime_ns, 'stamped before the key was held'\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
product = os.path.join(target, 'built')\n\
if os.path.exists(product):\n    assert stamp > os.lstat(product).st_mtime_ns\n\
open(product, 'w').write('x')\n";

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
    let output = crate::common::af()
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_STATE_HOME", fixture.home.join(".local/state"))
        .env("XDG_DATA_HOME", fixture.home.join(".local/share"))
        .env("XDG_CACHE_HOME", &fixture.cache)
        .env("PATH", format!("{}:/usr/bin:/bin", fixture.bin.display()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AF_CACHE_POLICY_FILE")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_HOME")
        .env_remove("RUSTUP_HOME")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE")
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
    schemas::valid(&schemas::validator("task-inspection-v13.json"), &outcome);
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
    assert!(shown.contains(", cargo_target cold empty"), "{shown}");
    let shown = show(&fixture, "warm-two");
    assert!(shown.contains(", cargo_target warm 4.0 KiB"), "{shown}");

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
fn a_warm_check_runs_on_sources_stamped_after_the_build_it_reuses() {
    let fixture = fixture(code_policy(
        STAMPED,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let first = start(&fixture, "stamped-one", 0);
    // Hold the already-warm key until the next check has probed its toolchain. A product
    // written three seconds after that probe is newer than a premature two-second source
    // stamp, so this catches a refresh placed before the lock wait as well as no refresh.
    let target = warm_directories(&fixture).pop().unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(target.parent().unwrap().join("warm.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let cargo = fixture.bin.join("cargo");
    let stub = std::fs::read_to_string(&cargo).unwrap();
    std::fs::write(
        &cargo,
        format!("{stub}\ntouch \"$(dirname \"$0\")/probed\"\n"),
    )
    .unwrap();
    let marker = fixture.bin.join("probed");
    let holder = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "next check never probed its toolchain"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
        // A holder that builds another Snapshot first removes the key's sources record, as the
        // kernel does (ADR-0207): the waiting check then finds no record and stamps every
        // source group after what the holder built.
        let key = target.parent().unwrap();
        let sources = key.parent().unwrap().join(format!(
            "{}.sources",
            key.file_name().unwrap().to_str().unwrap()
        ));
        assert!(sources.exists(), "the first check recorded what it built");
        std::fs::remove_file(sources).unwrap();
        std::fs::write(target.join("built"), b"x").unwrap();
        drop(lock);
    });
    let second = start(&fixture, "stamped-two", 0);
    holder.join().unwrap();
    assert_eq!(observations(&first)[0]["bytes_available"], 0);
    assert_eq!(
        observations(&second)[0]["bytes_available"],
        1,
        "the second check reused the first one's directory"
    );
    let cas = cas(&fixture);
    for outcome in [&first, &second] {
        assert_eq!(check_receipt(&cas, outcome)["outcome"], "passed");
    }
    let target = warm_directories(&fixture).pop().unwrap();
    let product = std::fs::File::open(target.join("built")).unwrap();
    product
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::now() + std::time::Duration::from_secs(86_400),
            ),
        )
        .unwrap();
    let third = start(&fixture, "stamped-future", 0);
    assert_eq!(check_receipt(&cas, &third)["outcome"], "passed");
    let observed = observations(&third);
    assert_eq!(observed[0]["kind"], "cargo_target:future_mtime");
    assert_eq!(observed[0]["eligible"], false);
    assert_eq!(observed[0]["bytes_available"], 0);
    assert_eq!(
        observed[0]["source_digest"],
        observations(&second)[0]["source_digest"]
    );
    assert!(
        product.metadata().unwrap().modified().unwrap() > std::time::SystemTime::now(),
        "the cold fallback did not rewrite the held target"
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
    assert_eq!(
        observed[0]["bound"], "max_bytes",
        "before a check only eviction acts"
    );
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
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096\nhard_max_bytes = 4096"),
    ));
    let started = std::time::Instant::now();
    let outcome = start(&fixture, "bound-grow", 5);
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
    assert_eq!(
        observed[0]["bound"], "hard_max_bytes",
        "the hard bound ended it"
    );
    assert!(
        warm_directories(&fixture).is_empty(),
        "the directory is gone"
    );
    let shown = show(&fixture, "bound-grow");
    assert!(
        shown.contains("cargo_target cold empty, removed 8.0 KiB (bound_exceeded hard_max_bytes)"),
        "{shown}"
    );
}

/// Grows its build directory past a 4096-byte `max_bytes` but under the default hard bound of
/// twice that, stays long enough for the monitor to sample it, and passes.
const SOFT_GROW: &str = "import os, time, pagination\n\
assert pagination.paginate(list(range(7)),2,3) == [2,3,4]\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
open(os.path.join(target, 'big.bin'), 'wb').write(b'x' * 6144)\n\
time.sleep(6)\n";

#[test]
fn a_check_past_only_the_eviction_bound_passes_and_the_next_check_finds_it_removed() {
    let policy = |script| {
        code_policy(
            script,
            120_000,
            false,
            Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096"),
        )
    };
    let fixture = fixture(policy(SOFT_GROW));
    let outcome = start(&fixture, "soft-grow", 0);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "passed", "the check's own result stands");
    assert!(result["reason"].is_null(), "{result}");
    assert_eq!(check_receipt(&cas, &outcome)["outcome"], "passed");
    assert_eq!(outcome["result"]["acceptance"], "satisfied");
    let observed = observations(&outcome);
    assert_eq!(observed.len(), 1, "{observed:?}");
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert_eq!(observed[0]["eligible"], true);
    assert_eq!(observed[0]["evicted_bytes"], 6144);
    assert_eq!(observed[0]["evicted_reason"], "bound_exceeded");
    assert_eq!(observed[0]["bound"], "max_bytes");
    assert!(
        warm_directories(&fixture).is_empty(),
        "evicted after the check"
    );
    let shown = show(&fixture, "soft-grow");
    assert!(
        shown.contains(
            "cargo_target cold empty, removed 6.0 KiB (bound_exceeded max_bytes); the next check \
             runs cold"
        ),
        "{shown}"
    );

    commit(&fixture, ".af/code-policy.toml", &policy(BUILD));
    let next = start(&fixture, "after-soft-grow", 0);
    let observed = observations(&next);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert_eq!(
        observed[0]["bytes_available"], 0,
        "the next check found the directory removed and ran cold"
    );
    assert!(observed[0].get("bound").is_none(), "{observed:?}");
    assert_eq!(warm_directories(&fixture).len(), 1, "warm again afterwards");
}

#[test]
fn a_check_that_removes_its_warm_root_fails_as_suspect_with_the_cause_recorded() {
    let fixture = fixture(code_policy(
        REMOVE_ROOT,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let outcome = start(&fixture, "remove-root", 5);
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
        shown.contains("cargo_target cold empty, removed 0 B (suspect)"),
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
    let outcome = start(&fixture, "replace-lock", 5);
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
    let outcome = start(&fixture, "widen-key", 5);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_suspect");
    assert!(warm_directories(&fixture).is_empty());
    let shown = show(&fixture, "widen-key");
    assert!(shown.contains("removed 0 B (suspect)"), "{shown}");
}

#[test]
fn a_check_that_makes_the_key_read_only_still_fails_and_is_evicted() {
    let fixture = fixture(code_policy(
        READ_ONLY_KEY,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    let outcome = start(&fixture, "read-only-key", 5);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_suspect");
    assert!(
        warm_directories(&fixture).is_empty(),
        "evicted through the held descriptor"
    );
}

#[test]
fn an_oversized_entry_with_a_non_utf8_name_is_counted_and_evicted() {
    // APFS refuses a name that is not UTF-8, so this fixture can only run where the filesystem
    // accepts one; the eviction-by-exact-bytes rule it pins matters on those systems.
    let probe = tempfile::tempdir().unwrap();
    if std::fs::write(
        probe.path().join(std::ffi::OsStr::from_bytes(b"\xff\xfe")),
        b"x",
    )
    .is_err()
    {
        return;
    }
    let fixture = fixture(code_policy(
        ODD_NAME,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096\nhard_max_bytes = 4096"),
    ));
    let outcome = start(&fixture, "odd-name", 5);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    assert_eq!(result["reason"], "warm_cache_bound_exceeded");
    assert!(warm_directories(&fixture).is_empty());
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
        shown.contains(", cargo_target warm 4.0 KiB, cargo_home warm 1.0 KiB"),
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

/// The implementer of `fixture` replaced by one that also removes the kernel's rustup home, as
/// a machine whose rustup home went away while a Task ran: the toolchain resolved when the plan
/// was admitted (ADR-0205) and no longer does when the check probes it.
fn removing_the_rustup_home_while_it_runs(fixture: &Fixture) {
    let implementer = fixture.repo.join(".af/task-packages/fixture/implementer");
    std::fs::write(
        implementer.join("worker.py"),
        format!(
            "import json,os,sys\njson.load(sys.stdin)\n\
             open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\n\
             os.rmdir({:?})\n\
             print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'report':[{{'summary':'Implemented pagination'}}]}}}}))\n",
            fixture.home.join(".rustup").display().to_string()
        ),
    )
    .unwrap();
    let catalog_path = fixture.repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"]["fixture/implementer"]["digest"] =
        toml::Value::String(package_digest("fixture/implementer", &implementer).unwrap());
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    git(&fixture.repo, &["add", "-A"]);
    git(
        &fixture.repo,
        &["commit", "-qm", "the rustup home goes away"],
    );
}

#[test]
fn without_a_rustup_home_the_probe_fails_and_the_check_runs_cold_with_its_reason() {
    let fixture = fixture(code_policy(
        BUILD,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]"),
    ));
    // Admission resolves the toolchain while the rustup home is there (ADR-0205); the check
    // runs after the implementer removed it.
    removing_the_rustup_home_while_it_runs(&fixture);
    let outcome = start(&fixture, "no-rustup", 0);
    assert!(!fixture.home.join(".rustup").exists());
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

/// A check whose toolchain does not resolve on this machine is refused before the plan is
/// admitted (ADR-0205): before the implementer runs, with the policy file and the field in the
/// message, and nothing charged. Here once with no rustup home at all, once with a Snapshot
/// that pins a toolchain the rustup home does not hold.
#[test]
fn a_check_whose_toolchain_does_not_resolve_is_refused_before_the_implementer_runs() {
    for (case, pinned) in [
        ("no-rustup-home", "1.88.0"),
        ("unavailable-toolchain", "9.9.9"),
    ] {
        let fixture = fixture(code_policy(
            BUILD,
            60_000,
            false,
            Some("build_cache = [\"cargo_target\"]"),
        ));
        if pinned == "1.88.0" {
            std::fs::remove_dir(fixture.home.join(".rustup")).unwrap();
        } else {
            commit(
                &fixture,
                "rust-toolchain.toml",
                &format!("[toolchain]\nchannel = \"{pinned}\"\n"),
            );
            // Like a rustup proxy whose home lacks the pinned channel, with auto-install off.
            let rustc = fixture.bin.join("rustc");
            let stub = std::fs::read_to_string(&rustc).unwrap();
            std::fs::write(
                &rustc,
                stub.replacen(
                    "#!/bin/sh\n",
                    &format!(
                        "#!/bin/sh\nif grep -q '{pinned}' rust-toolchain.toml 2>/dev/null; then \
                         echo \"error: toolchain '{pinned}' is not installed\" >&2; exit 1; fi\n"
                    ),
                    1,
                ),
            )
            .unwrap();
        }
        let file = fixture.root.join("tasks").join(format!("{case}.json"));
        std::fs::write(
            &file,
            serde_json::to_vec(&serde_json::json!({
                "schema": "af.task-file/1", "task_id": case, "kind": "implement",
                "goal": "Implement this Jira ticket: offset/limit pagination",
                "pipeline": {"name": "fixture/implementation", "fallback": "refuse"},
                "strategy": "small", "facts": {},
                "limits": {"tokens": 1000, "max_attempts": 3, "wall_ms": 600_000,
                    "verification": {"tokens": 200, "attempts": 2, "wall_ms": 300_000}}
            }))
            .unwrap(),
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
        assert_eq!(code, 1, "{case}: {stderr}\n{stdout}");
        let flat = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in [
            ".af/code-policy.toml: [warm] build_cache = [\"cargo_target\"] makes check \
             `pagination` resolve its Rust toolchain before it runs",
            &format!(
                "the toolchain `{pinned}` its Snapshot's rust-toolchain.toml pins does not resolve on this machine (rustc -vV exited with"
            ),
            &format!("`rustup toolchain install {pinned}`"),
            "nothing ran and nothing was charged",
        ] {
            assert!(
                flat.contains(needle),
                "{case}: `{needle}` missing:\n{stderr}"
            );
        }
        let (code, shown, stderr) = af(
            &fixture,
            &[
                "task",
                "show",
                case,
                "--state",
                fixture.state.to_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(code, 0, "{stderr}");
        let shown: Value = serde_json::from_str(shown.trim()).unwrap();
        assert_eq!(shown["attempts"], 0, "{case}: no Attempt ran: {shown}");
        assert!(
            shown.get("result").is_none_or(Value::is_null),
            "{case}: {shown}"
        );
    }
}

#[test]
fn a_fast_check_that_writes_past_the_bound_fails_and_its_directory_is_removed() {
    let fixture = fixture(code_policy(
        FAST_GROW,
        120_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 4096\nhard_max_bytes = 4096"),
    ));
    let outcome = start(&fixture, "fast-grow", 5);
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
        shown.contains("cargo_target cold empty, removed 8.0 KiB (bound_exceeded hard_max_bytes)"),
        "{shown}"
    );
}

#[test]
fn an_unreadable_subtree_a_check_leaves_fails_it_as_suspect() {
    if !permissions_bind() {
        return;
    }
    let fixture = fixture(code_policy(
        HIDE,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nmax_bytes = 1048576"),
    ));
    let outcome = start(&fixture, "hidden-grow", 5);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    assert_eq!(result["status"], "failed");
    // An unreadable subtree is suspect before it is a byte count: suspicion outranks the bound
    // in the recorded cause, and the directory goes either way.
    assert_eq!(result["reason"], "warm_cache_suspect");
    let observed = observations(&outcome);
    assert_eq!(observed[0]["kind"], "cargo_target");
    assert!(observed[0]["evicted_bytes"].is_u64());
    assert_eq!(observed[0]["evicted_reason"], "suspect");
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
    let outcome = start(&fixture, "refused-cache", 5);
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
    let outcome = start(&fixture, "deadline-skip", 5);
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
const NO_WARM_DOCUMENTS: &str = include_str!("../fixtures/task-warm/no-warm-documents.json");

/// Pair two independent runs' documents. A value equal in both is kept literally: the captured
/// policy, the Snapshots, the candidate and the check result bytes are content identities that
/// do not depend on when or where the Task ran. A value that differs between the runs — a
/// deadline and every identity derived from it, an Attempt ULID, a writer pid, a fixture path —
/// becomes `<run:N>`, N its first appearance, and the two runs must pair such values one to
/// one, so every equality between them survives. Host timings are masked first, because two
/// runs can agree on a duration by chance: the inspection's `clock/wall_ms` among them, the
/// time between the Task's opening and its deadline; so are the host's load and CPUs a check
/// report records (ADR-0205), which two runs share by chance and two machines need not.
#[derive(Default)]
struct RunIndependent {
    first: std::collections::BTreeMap<String, usize>,
    second: std::collections::BTreeMap<String, usize>,
}

impl RunIndependent {
    fn pair(&mut self, first: &Value, second: &Value, path: &str) -> Value {
        let key = path.rsplit('/').next().unwrap_or(path);
        match (first, second) {
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(
                    a.keys().collect::<Vec<_>>(),
                    b.keys().collect::<Vec<_>>(),
                    "two runs record the same fields at {path}"
                );
                Value::Object(
                    a.iter()
                        .map(|(name, value)| {
                            let path = format!("{path}/{name}");
                            (name.clone(), self.pair(value, &b[name], &path))
                        })
                        .collect(),
                )
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(
                    a.len(),
                    b.len(),
                    "two runs record as many entries at {path}"
                );
                Value::Array(
                    a.iter()
                        .zip(b)
                        .map(|(a, b)| self.pair(a, b, path))
                        .collect(),
                )
            }
            _ if key.ends_with("_unix_ms")
                || key == "elapsed_ms"
                || path.ends_with("/clock/wall_ms") =>
            {
                Value::String("<ms>".into())
            }
            _ if key.starts_with("load_1m_") || key.starts_with("cpus_") => {
                Value::String("<host>".into())
            }
            _ if first == second => first.clone(),
            _ => {
                let next = self.first.len();
                let a = *self.first.entry(first.to_string()).or_insert(next);
                let b = *self.second.entry(second.to_string()).or_insert(next);
                assert_eq!(a, b, "run-dependent values pair one to one at {path}");
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
    let mut inspection: Value = serde_json::from_str(shown.trim()).unwrap();
    // The next command carries the `--state` the command was given, a path of this run: the
    // documents compare the command the state chose.
    let selector = format!(" --state {}", fixture.state.display());
    let next = inspection["state"]["next_action"]
        .as_str()
        .unwrap()
        .to_owned();
    let chosen = next
        .strip_suffix(&selector)
        .unwrap_or_else(|| panic!("{next}"));
    inspection["state"]["next_action"] = chosen.into();
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

/// A native Rust toolchain the operator's mapping names: a stub `rustc` and `cargo` that answer
/// the version probes with release 1.88.0, copied privately into every check's runtime directory
/// (ADR-0127). `variant` changes the copied bytes, so the verified digest, and nothing else.
fn native_toolchain(fixture: &Fixture, variant: &str) -> PathBuf {
    let source = fixture.root.join(format!("native-{variant}"));
    std::fs::create_dir_all(source.join("bin")).unwrap();
    for (program, text) in [
        (
            "rustc",
            "rustc 1.88.0 (native fixture)\\nrelease: 1.88.0\\nhost: fixture-native-host",
        ),
        ("cargo", "cargo 1.88.0 (native fixture)"),
    ] {
        let path = source.join("bin").join(program);
        std::fs::write(
            &path,
            format!("#!/bin/sh\n# {variant}\nprintf '{text}\\n'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let limits = review_sandbox::toolchain::ToolchainLimits {
        max_bytes: 1_000_000,
        max_entries: 100,
        max_copy_bytes: 1_000_000,
    };
    let seed = fixture.root.join(format!("seed-{variant}"));
    let digest =
        review_sandbox::toolchain::snapshot_toolchain(&source, &seed, limits, None).unwrap();
    std::fs::remove_dir_all(seed).unwrap();
    let mapping = fixture.root.join(format!("rust-toolchain-{variant}.toml"));
    std::fs::write(
        &mapping,
        format!(
            "version = 1\n[rust]\nversion = '1.88.0'\nhost = 'fixture-native-host'\n\
             components = []\nsource = '{}'\nexpected_digest = '{digest}'\nmax_bytes = 1000000\n\
             max_entries = 100\nmax_copy_bytes = 1000000\n",
            source.display()
        ),
    )
    .unwrap();
    mapping
}

const NATIVE: &str = "\n[rust_toolchain]\nversion = \"1.88.0\"\nhost = \"fixture-native-host\"\n\
components = []\nchecks = [\"pagination\"]\n";

/// ADR-0144 D1: a prepared native toolchain lives in each check's own runtime directory, and
/// that path no longer reaches the key: two Tasks share one key and the second check is warm.
/// A toolchain with another verified digest is another key.
#[test]
fn a_native_toolchain_check_keeps_one_warm_key_across_tasks() {
    let fixture = fixture(format!(
        "{}{NATIVE}",
        code_policy(
            BUILD,
            60_000,
            false,
            Some("build_cache = [\"cargo_target\"]")
        )
    ));
    let mapping = native_toolchain(&fixture, "first");
    let extra = [("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE", mapping.as_path())];
    let first = start_with(&fixture, &extra, "native-one", 0);
    let second = start_with(&fixture, &extra, "native-two", 0);
    let cold = observations(&first);
    let warm = observations(&second);
    assert!(cold[0]["toolchain_id"].is_string(), "{cold:?}");
    assert_eq!(warm[0]["toolchain_id"], cold[0]["toolchain_id"], "one key");
    assert_eq!(cold[0]["bytes_available"], 0);
    assert_eq!(warm[0]["bytes_available"], 4096, "the second check is warm");
    assert_eq!(
        warm_directories(&fixture).len(),
        1,
        "no second key was made"
    );
    let shown = show(&fixture, "native-two");
    assert!(shown.contains("cargo_target warm 4.0 KiB"), "{shown}");

    // Other toolchain bytes are another verified digest, so another key.
    let changed = native_toolchain(&fixture, "second");
    let other = start_with(
        &fixture,
        &[("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE", changed.as_path())],
        "native-three",
        0,
    );
    let third = observations(&other);
    assert_ne!(third[0]["toolchain_id"], cold[0]["toolchain_id"]);
    assert_eq!(third[0]["bytes_available"], 0, "a new key starts cold");
    assert_eq!(warm_directories(&fixture).len(), 2);
}

/// Finding C1: `[task] rust_toolchain_policy` in the user's configuration selects the native
/// toolchain mapping with no variable exported: the gate resolves the mapped toolchain, and the
/// variable naming the same mapping selects the same warm key, so its check is warm.
#[test]
fn a_configured_toolchain_policy_resolves_the_native_toolchain_without_the_variable() {
    let fixture = fixture(format!(
        "{}{NATIVE}",
        code_policy(
            BUILD,
            60_000,
            false,
            Some("build_cache = [\"cargo_target\"]")
        )
    ));
    let mapping = native_toolchain(&fixture, "first");
    let config = fixture.home.join(".config/af/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!(
            "[task]\nrust_toolchain_policy = \"{}\"\n",
            mapping.display()
        ),
    )
    .unwrap();
    let configured = start(&fixture, "configured-one", 0);
    let cold = observations(&configured);
    assert!(cold[0]["toolchain_id"].is_string(), "{cold:?}");
    assert_eq!(cold[0]["bytes_available"], 0);
    let extra = [("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE", mapping.as_path())];
    let variable = start_with(&fixture, &extra, "variable-two", 0);
    let warm = observations(&variable);
    assert_eq!(warm[0]["toolchain_id"], cold[0]["toolchain_id"], "one key");
    assert_eq!(warm[0]["bytes_available"], 4096, "the second check is warm");
}

/// A check that reports the four directories af gave it, and whether each was empty and
/// writable, into `report` outside its tree.
fn runtime_probe(report: &Path) -> String {
    format!(
        "import json, os\n\
names = ['HOME', 'TMPDIR', 'AF_CHECK_SCRATCH', 'XDG_CACHE_HOME']\n\
seen = {{}}\n\
for name in names:\n    path = os.environ.get(name)\n    entry = {{'path': path}}\n    if path:\n        entry['empty'] = os.listdir(path) == []\n        probe = os.path.join(path, 'probe')\n        open(probe, 'w').write('x')\n        entry['writable'] = open(probe).read() == 'x'\n    seen[name] = entry\n\
open({report:?}, 'w').write(json.dumps(seen))\n\
import pagination\n\
assert pagination.paginate(list(range(7)),2,3) == [2,3,4]\n",
        report = report.display().to_string()
    )
}

/// ADR-0144 D4: every check gets a HOME, a TMPDIR, an AF_CHECK_SCRATCH and an XDG_CACHE_HOME,
/// each empty and writable, all below one `af-check-<pid>-…` runtime directory that is gone once
/// the check ended.
#[test]
fn a_check_gets_four_private_directories_that_are_gone_after_it() {
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().canonicalize().unwrap().join("report.json");
    let fixture = fixture(code_policy(&runtime_probe(&report), 60_000, false, None));
    let done = start(&fixture, "runtime-dirs", 0);
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let seen: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    let home = PathBuf::from(seen["HOME"]["path"].as_str().unwrap());
    let runtime = home.parent().unwrap().to_path_buf();
    let name = runtime.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("af-check-"), "{name}");
    for (variable, below) in [
        ("HOME", "home"),
        ("TMPDIR", "tmp"),
        ("AF_CHECK_SCRATCH", "scratch"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        let entry = &seen[variable];
        assert_eq!(
            PathBuf::from(entry["path"].as_str().unwrap()),
            runtime.join(below),
            "{variable}"
        );
        assert_eq!(entry["empty"], true, "{variable}");
        assert_eq!(entry["writable"], true, "{variable}");
    }
    assert!(
        !runtime.exists(),
        "the runtime directory outlived its check"
    );
}

/// A check that reports its build step after a second, then runs its "tests" for a second and
/// a half, and leaves one build product (ADR-0207).
const PHASED: &str = "import json, os, time\n\
time.sleep(1.2)\n\
step = {'schema': 'af.check-report/1', 'record': 'step', 'label': 'test-build', 'stage': 'build', 'exit_code': 0, 'command': ['true']}\n\
open(os.environ['AF_CHECK_REPORT'], 'a').write(json.dumps(step) + '\\n')\n\
time.sleep(1.5)\n\
target = os.environ['CARGO_TARGET_DIR']\n\
os.makedirs(target, exist_ok=True)\n\
open(os.path.join(target, 'built'), 'w').write('x')\n";

/// A check that, during its build, has its layer evicted the way the Storage Budget evicts an
/// idle key: the key directory removed and the sweep's eviction record left beside it. It then
/// fails, as a build that lost its directory does.
const EVICTED: &str = "import json, os, shutil, time\n\
key = os.path.dirname(os.environ['CARGO_TARGET_DIR'])\n\
shutil.rmtree(key)\n\
record = {'schema': 'af.warm-eviction/1', 'by': 'storage_sweep', 'at_unix_ms': int(time.time() * 1000), 'bytes': 0}\n\
open(key + '.evicted', 'w').write(json.dumps(record))\n\
time.sleep(6)\n\
raise SystemExit(1)\n";

fn show_checks(fixture: &Fixture, task_id: &str) -> (String, Value) {
    let args = |json: bool| {
        let mut args = vec![
            "task",
            "show",
            task_id,
            "--checks",
            "--state",
            fixture.state.to_str().unwrap(),
        ];
        if json {
            args.push("--json");
        }
        args.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let text = {
        let args = args(false);
        let (_, stdout, _) = af(
            fixture,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        stdout
    };
    let args = args(true);
    let (_, stdout, stderr) = af(
        fixture,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let document: Value =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|_| panic!("{stderr}"));
    schemas::valid(&schemas::validator("task-checks-v2.json"), &document);
    (text, document)
}

/// Digits replaced, so a snapshot of `af task show --checks` holds still across runs.
fn without_digits(text: &str) -> String {
    let mut out = String::new();
    let mut digits = false;
    for c in text.chars() {
        if c.is_ascii_hexdigit() && c.is_ascii_digit() {
            if !digits {
                out.push('N');
            }
            digits = true;
        } else {
            digits = false;
            out.push(c);
        }
    }
    out
}

/// ADR-0207, items 3 and 6: a warm check with a declared build deadline records its layer and
/// both phases beside its receipt; `af task show --checks` prints them, and the same content
/// the second time starts warm with every source group fresh.
#[test]
fn a_warm_check_records_its_layer_and_both_phases_and_show_checks_prints_them() {
    let fixture = fixture(code_policy(
        PHASED,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nbuild_wall_ms = 60000"),
    ));
    start(&fixture, "phased-one", 0);
    let (text, document) = show_checks(&fixture, "phased-one");
    let check = &document["receipt"]["checks"][0];
    assert_eq!(check["status"], "passed", "{text}");
    let layer = &check["layer"];
    assert_eq!(layer["lock"], "held");
    assert_eq!(layer["start"], "cold");
    assert_eq!(
        layer["sources"],
        serde_json::json!({"stale": 1, "fresh": 0})
    );
    assert!(layer["key"].as_str().unwrap().contains('/'));
    let phases = &layer["phases"];
    assert_eq!(phases["build_wall_ms"], 60_000);
    assert_eq!(phases["check_wall_ms"], 60_000);
    let build = phases["build_ms"].as_u64().unwrap();
    let tests = phases["tests_ms"].as_u64().unwrap();
    assert!(build >= 1_200, "the build step came after 1.2 s: {phases}");
    assert_eq!(build + tests, phases["elapsed_ms"].as_u64().unwrap());
    assert!(phases.get("deadline").is_none());
    let lines: Vec<String> = text
        .lines()
        .filter(|line| line.starts_with("  layer ") || line.starts_with("  phases: "))
        .map(without_digits)
        .collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].ends_with(": cold at start, held at once; source groups N stale, N fresh"),
        "{text}"
    );
    assert_eq!(
        lines[1], "  phases: build N ms of N ms, tests N ms of N ms",
        "{text}"
    );

    start(&fixture, "phased-two", 0);
    let (_, document) = show_checks(&fixture, "phased-two");
    let layer = &document["receipt"]["checks"][0]["layer"];
    assert_eq!(
        layer["start"], "warm",
        "the second check reused the first one's build"
    );
    assert_eq!(
        layer["sources"],
        serde_json::json!({"stale": 0, "fresh": 1}),
        "the same content keeps the build"
    );
}

/// ADR-0207, item 3: a layer evicted under a running build is named as the cause, with its
/// phase, its evictor and its time, in the result's reason and in the layer record.
#[test]
fn a_layer_evicted_during_the_build_names_the_sweep_as_the_cause() {
    let fixture = fixture(code_policy(
        EVICTED,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\"]\nbuild_wall_ms = 60000"),
    ));
    let outcome = start(&fixture, "evicted", 5);
    let cas = cas(&fixture);
    let result = check_result(&cas, &outcome);
    let reason = result["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("layer evicted during build by storage sweep at "),
        "{reason}"
    );
    let (text, document) = show_checks(&fixture, "evicted");
    let evicted = &document["receipt"]["checks"][0]["layer"]["evicted"];
    assert_eq!(evicted["phase"], "build");
    assert_eq!(evicted["by"], "storage_sweep");
    assert!(
        text.lines()
            .any(|line| line.starts_with("  layer evicted during build by storage sweep at ")),
        "{text}"
    );
}

/// The check of an offline policy: npm reads only its cache, every proxy names a closed port,
/// and an attempt to reach the registry fails at once (ADR-0207, item 4).
const OFFLINE: &str = "import os, urllib.request\n\
assert os.environ['npm_config_offline'] == 'true'\n\
for name in ('HTTP_PROXY', 'HTTPS_PROXY', 'http_proxy', 'https_proxy'):\n    assert os.environ[name] == 'http://127.0.0.1:9', name\n\
cache = os.environ['npm_config_cache']\n\
assert os.path.isdir(os.path.join(cache, '_cacache')), cache\n\
os.makedirs(os.path.join(cache, '_npx', 'tree', 'node_modules', '.bin'), exist_ok=True)\n\
link = os.path.join(cache, '_npx', 'tree', 'node_modules', '.bin', 'tool')\n\
if not os.path.islink(link):\n    os.symlink('../tool/cli.js', link)\n\
try:\n    urllib.request.urlopen('http://registry.npmjs.org/', timeout=10)\n    raise SystemExit('the network was reachable')\n\
except OSError:\n    pass\n";

/// ADR-0207, item 4: an offline check runs with `HTTP_PROXY` on a dead port and passes on a
/// pinned package its npm cache holds; the cache is filled once, by `npm exec` with the
/// network, and a cached tarball whose bytes differ from the pin is refused before the check.
#[test]
fn an_offline_check_passes_with_a_dead_proxy_on_a_pinned_package_filled_once() {
    use sha2::Digest;
    let tarball = b"markdownlint-cli2 0.22.1 tarball bytes".as_slice();
    let sha256: String = sha2::Sha256::digest(tarball)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let policy = code_policy(
        OFFLINE,
        60_000,
        false,
        Some("build_cache = [\"cargo_target\", \"npm_cache\"]"),
    )
    .replacen("required = true\n", "required = true\noffline = true\n", 1)
        + &format!(
            "\n[[checks.pagination.npm_packages]]\nspec = \"markdownlint-cli2@0.22.1\"\n\
             sha256 = \"{sha256}\"\n"
        );
    let fixture = fixture(policy);
    // What npm would fetch: the pinned tarball, filed as npm's cache files it.
    let seed = fixture.root.join("npm-seed");
    review_pipeline::task::warm_check::offline::cache_tarball(
        &seed,
        &review_check_pin("markdownlint-cli2@0.22.1", &sha256),
        tarball,
    )
    .unwrap();
    let calls = fixture.root.join("npm-calls");
    let npm = fixture.bin.join("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\n[ \"$1\" = exec ] || exit 2\n\
             [ -z \"$npm_config_offline\" ] || exit 3\n\
             cp -R '{}/.' \"$npm_config_cache/\"\n",
            calls.display(),
            seed.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

    let first = start(&fixture, "offline-one", 0);
    let cas = cas(&fixture);
    assert_eq!(check_receipt(&cas, &first)["outcome"], "passed");
    let filled = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(
        filled.lines().collect::<Vec<_>>(),
        ["exec --yes --ignore-scripts --package=markdownlint-cli2@0.22.1 -- true"],
        "filled once"
    );
    let cache = warm_directories_of(&fixture, "npm_cache").pop().unwrap();
    assert!(cache.join("_cacache").is_dir(), "the layer holds the cache");
    assert!(
        !cache.join("_npx").exists(),
        "npm's install tree, with its links, is removed"
    );

    let second = start(&fixture, "offline-two", 0);
    assert_eq!(check_receipt(&cas, &second)["outcome"], "passed");
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap().lines().count(),
        1,
        "a filled cache never needs the network again"
    );

    // Other bytes under the pinned tarball's name: refused before the check runs.
    for entry in walk(&cache.join("_cacache/content-v2")) {
        std::fs::write(entry, b"other bytes").unwrap();
    }
    let third = start(&fixture, "offline-tampered", 5);
    let result = check_result(&cas, &third);
    assert_eq!(result["status"], "not_run");
    let reason = result["reason"].as_str().unwrap();
    assert!(
        reason.contains("markdownlint-cli2@0.22.1") && reason.contains("pins"),
        "{reason}"
    );
}

fn review_check_pin(spec: &str, sha256: &str) -> review_pipeline::task::warm_check::NpmPin {
    review_pipeline::task::warm_check::NpmPin {
        spec: spec.into(),
        sha256: sha256.into(),
    }
}

/// Every regular file below `root`.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files
}
