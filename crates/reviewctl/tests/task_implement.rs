//! v2 implement Task: source checkout remains untouched and the only successful output is a
//! materializable, independently evaluated internal Snapshot.

use std::path::{Path, PathBuf};
use std::process::Command;

use review_config::lock::package_digest;
use review_source_git::Manifest;
use review_store::Cas;

fn git(repo: &Path, home: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_package(root: &Path, name: &str, script: &str, prompt: &str) {
    let package = root.join(name);
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        format!(
            "name = \"{name}\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n\
             [runner]\nprogram = \"/bin/sh\"\nargs = [{{ value = \"-c\" }}, {{ value = '''{script}''' }}]\n"
        ),
    )
    .unwrap();
    std::fs::write(package.join("reviewer.md"), prompt).unwrap();
}

fn fixture(root: &Path, passing_gate: bool) -> (PathBuf, PathBuf, PathBuf) {
    let repo = root.join("repo");
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(repo.join(".af/pipelines")).unwrap();
    std::fs::create_dir_all(repo.join(".af/workers")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(repo.join("seed.txt"), "source\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "*.generated\n").unwrap();
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[defaults]\npipeline = \"review\"\ntask_pipeline = \"implement\"\n",
    )
    .unwrap();
    write_package(
        &repo.join(".af/workers"),
        "implementer",
        "printf 'derived\\n' > implemented.txt; printf 'ignored but delivered\\n' > proof.generated; printf 'done'",
        "Implement the exact goal in the sandbox.",
    );
    write_package(
        &repo.join(".af/workers"),
        "evaluator",
        r#"printf '{"verdict":"approve","summary":"independent pass"}'"#,
        "Evaluate the goal against the provided Snapshot. Return only the requested JSON.",
    );
    let gate_script = if passing_gate {
        "test \"$(cat implemented.txt)\" = derived"
    } else {
        "exit 7"
    };
    let pipeline = format!(
        "version = 1\nkind = \"implement\"\nimplementer = \"implementer\"\n\
         evaluator = \"evaluator\"\ntimeout_seconds = 10\ncheck_timeout_seconds = 10\n\n\
         attempt_tokens = 1000\nrun_tokens = 2000\n\n[[checks]]\nname = \"acceptance\"\nprogram = \"/bin/sh\"\n\
         args = [{{ value = \"-c\" }}, {{ value = '''{gate_script}''' }}]\n"
    );
    std::fs::write(repo.join(".af/pipelines/implement.toml"), &pipeline).unwrap();
    let implementer = package_digest("implementer", &repo.join(".af/workers/implementer")).unwrap();
    let evaluator = package_digest("evaluator", &repo.join(".af/workers/evaluator")).unwrap();
    let pipeline_digest = review_store::canonical::blob_content_id(pipeline.as_bytes());
    std::fs::write(
        repo.join(".af/af.lock"),
        format!(
            "version = 1\n\n[workers.implementer]\nversion = \"1.0.0\"\ndigest = \"{implementer}\"\n\n\
             [workers.evaluator]\nversion = \"1.0.0\"\ndigest = \"{evaluator}\"\n\n\
             [pipelines.implement]\nversion = \"1.0.0\"\ndigest = \"{pipeline_digest}\"\n"
        ),
    )
    .unwrap();
    git(&repo, &home, &["init", "-q", "-b", "main"]);
    git(&repo, &home, &["config", "user.email", "t@t.invalid"]);
    git(&repo, &home, &["config", "user.name", "T"]);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "initial"]);
    (repo, home, state)
}

fn run_task(repo: &Path, home: &Path, state: &Path) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .env("HOME", home)
        .args([
            "task",
            "start",
            "--kind",
            "implement",
            "--goal",
            "create implemented.txt",
            "--authority",
            "HEAD",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run_af(repo: &Path, home: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn verified_task_ends_at_a_materializable_internal_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(outcome["schema"], "af/task-outcome@1");
    assert_eq!(outcome["outcome"]["kind"], "verified");
    assert_eq!(outcome["delivery"]["kind"], "none");
    assert_eq!(outcome["workers"].as_array().unwrap().len(), 2);
    assert_eq!(outcome["gates"][0]["status"], "passed");
    assert!(
        !repo.join("implemented.txt").exists(),
        "source checkout changed"
    );
    assert!(state.join("tasks.sqlite").exists());

    let cas = Cas::open(state.join("cas")).unwrap();
    let snapshot_id = outcome["derived_snapshot_id"].as_str().unwrap();
    let snapshot = cas.get_json(snapshot_id).unwrap();
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(snapshot["manifest_artifact_id"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    let materialized = tempfile::tempdir().unwrap();
    review_source_git::materialize(&manifest, &cas, materialized.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(materialized.path().join("implemented.txt")).unwrap(),
        "derived\n"
    );

    let evaluator = &outcome["workers"][1];
    let task_input = evaluator["context_manifest"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "task_input")
        .unwrap();
    let evaluator_input: serde_json::Value = serde_json::from_slice(
        &cas.get(task_input["artifact_id"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    assert!(evaluator_input.get("goal").is_some());
    assert!(evaluator_input.get("gates").is_some());
    assert!(evaluator_input.get("implementer_output").is_none());
    assert!(evaluator_input.get("implementer_transcript").is_none());
}

#[test]
fn failed_gate_is_typed_unverified_and_skips_the_evaluator() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), false);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 3, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(outcome["outcome"]["kind"], "unverified");
    assert_eq!(outcome["outcome"]["stage"], "gates");
    assert_eq!(outcome["workers"].as_array().unwrap().len(), 1);
    assert_eq!(outcome["gates"][0]["status"], "failed");
    assert!(!repo.join("implemented.txt").exists());
}

#[test]
fn verified_task_delivery_is_local_exact_recoverable_and_inspectable() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let task_id = outcome["task_id"].as_str().unwrap();
    let worktree = directory.path().join("delivered");
    let delivery_args = [
        "task",
        "deliver",
        task_id,
        "--repo",
        repo.to_str().unwrap(),
        "--branch",
        "af/pilot",
        "--worktree",
        worktree.to_str().unwrap(),
        "--confirm",
        task_id,
        "--state",
        state.to_str().unwrap(),
        "--json",
    ];
    let (code, stdout, stderr) = run_af(&repo, &home, &delivery_args);
    assert_eq!(code, 0, "{stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["schema"], "af/task-delivery@1");
    assert_eq!(receipt["outcome"]["kind"], "delivered");
    assert_eq!(receipt["remote_actions"], serde_json::json!([]));
    assert_eq!(
        std::fs::read_to_string(worktree.join("implemented.txt")).unwrap(),
        "derived\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("proof.generated")).unwrap(),
        "ignored but delivered\n"
    );
    assert!(!repo.join("implemented.txt").exists());
    let source_status = Command::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(source_status.status.success());
    assert!(source_status.stdout.is_empty(), "source checkout changed");

    // Simulate a crash after exact materialization but before the terminal receipt. The durable
    // prepared record must let the exact repeat reconcile and seal the delivery without rewriting.
    let connection = rusqlite::Connection::open(state.join("tasks.sqlite")).unwrap();
    connection
        .execute(
            "DELETE FROM task_events WHERE task_id = ?1 AND event_type = 'TaskDelivered@1'",
            [task_id],
        )
        .unwrap();
    let (code, recovered, stderr) = run_af(&repo, &home, &delivery_args);
    assert_eq!(code, 0, "{stderr}");
    let recovered: serde_json::Value = serde_json::from_str(recovered.trim()).unwrap();
    assert_eq!(recovered["delivery_id"], receipt["delivery_id"]);

    let (code, repeated, stderr) = run_af(&repo, &home, &delivery_args);
    assert_eq!(code, 0, "{stderr}");
    let repeated: serde_json::Value = serde_json::from_str(repeated.trim()).unwrap();
    assert_eq!(repeated, recovered, "exact repeat must be idempotent");

    let (code, listed, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "list",
            "--repo",
            repo.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let listed: serde_json::Value = serde_json::from_str(listed.trim()).unwrap();
    assert_eq!(listed["tasks"][0]["task_id"], task_id);
    assert_eq!(listed["tasks"][0]["outcome"], "verified");
    assert_eq!(
        listed["tasks"][0]["delivery"]["outcome"]["kind"],
        "delivered"
    );

    let (code, shown, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "show",
            task_id,
            "--repo",
            repo.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    let shown: serde_json::Value = serde_json::from_str(shown.trim()).unwrap();
    assert_eq!(shown["schema"], "af/task-inspection@1");
    assert_eq!(shown["outcome"]["outcome"]["kind"], "verified");
    assert!(shown["history"].as_array().unwrap().len() >= 8);
}

#[test]
fn delivery_refuses_unverified_and_dirty_sources_without_creating_a_target() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), false);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 3, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let task_id = outcome["task_id"].as_str().unwrap();
    let refused = directory.path().join("refused");
    let (code, _, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "deliver",
            task_id,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/refused",
            "--worktree",
            refused.to_str().unwrap(),
            "--confirm",
            task_id,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("only a verified Task"), "{stderr}");
    assert!(!refused.exists());

    let clean = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(clean.path(), true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let task_id = outcome["task_id"].as_str().unwrap();
    let lock = rusqlite::Connection::open(state.join("task-delivery-lock.sqlite")).unwrap();
    lock.busy_timeout(std::time::Duration::from_millis(10))
        .unwrap();
    lock.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS delivery_lock (singleton INTEGER PRIMARY KEY);
         BEGIN IMMEDIATE;",
    )
    .unwrap();
    let busy = clean.path().join("busy-refused");
    let (code, _, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "deliver",
            task_id,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/busy-refused",
            "--worktree",
            busy.to_str().unwrap(),
            "--confirm",
            task_id,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("another Task delivery is active"),
        "{stderr}"
    );
    assert!(!busy.exists());
    drop(lock);

    std::fs::write(repo.join("local.txt"), "operator work\n").unwrap();
    let refused = clean.path().join("dirty-refused");
    let (code, _, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "deliver",
            task_id,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/dirty-refused",
            "--worktree",
            refused.to_str().unwrap(),
            "--confirm",
            task_id,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 1);
    assert!(stderr.contains("not clean"), "{stderr}");
    assert!(!refused.exists());
}

#[cfg(unix)]
#[test]
fn failed_local_creation_rolls_back_only_its_owned_refs() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let (repo, home, state) = fixture(directory.path(), true);
    let (code, stdout, stderr) = run_task(&repo, &home, &state);
    assert_eq!(code, 0, "{stderr}");
    let outcome: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let task_id = outcome["task_id"].as_str().unwrap();
    let locked = directory.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    let worktree = locked.join("delivery");
    let (code, _, stderr) = run_af(
        &repo,
        &home,
        &[
            "task",
            "deliver",
            task_id,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/rollback",
            "--worktree",
            worktree.to_str().unwrap(),
            "--confirm",
            task_id,
            "--state",
            state.to_str().unwrap(),
        ],
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code, 1);
    assert!(stderr.contains("was rolled back"), "{stderr}");
    assert!(!worktree.exists());
    let branch = Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/af/rollback"])
        .output()
        .unwrap();
    assert!(
        !branch.status.success(),
        "delivery branch survived rollback"
    );
    let connection = rusqlite::Connection::open(state.join("tasks.sqlite")).unwrap();
    let terminal: String = connection
        .query_row(
            "SELECT event_type FROM task_events WHERE task_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(terminal, "TaskDeliveryFailed@1");
}

#[test]
fn checked_in_task_authority_is_fully_pinned() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let implementer = package_digest("implementer", &root.join(".af/workers/implementer")).unwrap();
    let evaluator = package_digest("evaluator", &root.join(".af/workers/evaluator")).unwrap();
    let pipeline = review_store::canonical::blob_content_id(
        &std::fs::read(root.join(".af/pipelines/implement.toml")).unwrap(),
    );
    let lock: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join(".af/af.lock")).unwrap()).unwrap();
    println!("implementer={implementer}\nevaluator={evaluator}\npipeline={pipeline}");
    assert_eq!(
        lock["workers"]["implementer"]["digest"].as_str(),
        Some(implementer.as_str())
    );
    assert_eq!(
        lock["workers"]["evaluator"]["digest"].as_str(),
        Some(evaluator.as_str())
    );
    assert_eq!(
        lock["pipelines"]["implement"]["digest"].as_str(),
        Some(pipeline.as_str())
    );
}
