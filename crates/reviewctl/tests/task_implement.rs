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
    std::fs::write(
        repo.join(".af/af.toml"),
        "version = 1\n[defaults]\npipeline = \"review\"\ntask_pipeline = \"implement\"\n",
    )
    .unwrap();
    write_package(
        &repo.join(".af/workers"),
        "implementer",
        "printf 'derived\\n' > implemented.txt; printf 'done'",
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
