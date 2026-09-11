//! Public Task-file path: captured authority survives CLI process boundaries and edits.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    fixture_named(root, "pagination")
}

fn fixture_named(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let repo = root.join("repo");
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    copy_tree(&workspace.join("fixtures/task-runtime").join(name), &repo);
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
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
    (repo, root.join("state"))
}

#[test]
fn review_file_uses_common_task_state_and_keeps_changes_requested_exit() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review");
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(result["result"]["domain_conclusion"], "changes_requested");
    assert_eq!(result["attempts"], 3);
    assert_eq!(
        result["review_rounds"][0]["selected_results"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert!(state.join("events.sqlite").is_file());
    assert!(!state.join("tasks.sqlite").exists());
    let replay = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args(["task", "run", "review-cli", "--json", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(replay.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        result
    );
    let wrong_kind = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "review",
            "run",
            "--file",
            "review.json",
            "--heavy",
            "--json",
            "--state",
        ])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(wrong_kind.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn native_model_cli_admission_is_shared_and_account_changes_refuse_dispatch() {
    use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review");
    let home = directory.path().join("home");
    let bin = directory.path().join("bin");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let email = home.join("account-email");
    std::fs::write(&email, "developer@example.test").unwrap();
    let calls = home.join("calls");
    let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':open(home+'/account-email').read()}))
 sys.exit(0)
request=sys.stdin.read()
with open(home+'/calls','a') as f: f.write('model\n')
if request=='Reply with exactly: OK\n':
 result='OK'
else:
 value=json.loads(request)
 assert set(value['inputs'])=={'source','subject','history','checks'}
 result=json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'verdict':'approve','summary':'Checked source','reports':[],'benchmark_demands':[],'disputes':[]}]}})
print(json.dumps({'is_error':False,'result':result,'usage':{'input_tokens':10,'output_tokens':2,'cache_creation_input_tokens':0}}))
"#;
    std::fs::write(bin.join("claude"), stub).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
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
            toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap()).unwrap();
        worker.runner = TaskWorkerRunner::Model {
            provider_kind: "claude".into(),
            model: "claude-fixture-1".into(),
            effort: "high".into(),
        };
        worker.signature.attempt.as_mut().unwrap().tokens = 1000;
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
    file["limits"] = serde_json::json!({"tokens":10000,"max_attempts":4,"wall_ms":90000,"verification":{"tokens":6096,"attempts":4,"wall_ms":60000}});
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
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .env("HOME", &home)
            .env("USER", "fixture")
            .env("PATH", &path)
            .env("AF_PROVIDERS_FILE", &registry)
            .args(args)
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap()
    };
    let plan = run(&["review", "plan", "--file", "review.json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(!calls.exists(), "Planning made a model call");
    let planned: Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(planned["attempts"], 0);
    assert!(planned["graph"]["nodes"]["root.providers.admit0"].is_object());
    assert!(
        planned["graph"]["nodes"]["root.providers.admit1"].is_null(),
        "Identical capabilities did not share admission"
    );
    std::fs::write(&email, "changed@example.test").unwrap();
    let changed = run(&["task", "run", "review-cli"]);
    assert!(!changed.status.success());
    assert!(!calls.exists(), "Changed account reached model dispatch");
    std::fs::write(&email, "developer@example.test").unwrap();
    let output = run(&["task", "run", "review-cli"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["attempts"], 4);
    assert_eq!(result["chargeable_tokens"], 36);
    assert_eq!(result["result"]["domain_conclusion"], "pass");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 3);
    assert!(run(&["task", "run", "review-cli"]).status.success());
    assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 3);
}

fn af(repo: &Path, state: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(["task"])
        .args(args)
        .arg("--state")
        .arg(state)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "af task {args:?}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn embedded_review_never_accepts_findings_missing_reviewers_or_failed_checks() {
    for case in [
        "finding",
        "missing",
        "failed_checks",
        "missing_coverage",
        "stale_checks",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "embedded-review");
        let packages = repo.join(".af/task-packages/fixture");
        match case {
            "finding" => {
                let reply = serde_json::json!({"schema":"af.worker-reply/1","outputs":{"result":[{"verdict":"request-changes","summary":"Required case is missing","reports":[{"severity":"major","file":"pagination.py","line":1,"title":"Missing validation","body":"Offset must reject negative values","fix":"Validate offset","confidence":0.9}],"benchmark_demands":[],"disputes":[]}]}});
                std::fs::write(
                    packages.join("correctness/worker.py"),
                    format!(
                        "import json,sys\njson.load(sys.stdin)\nprint({:?})\n",
                        reply.to_string()
                    ),
                )
                .unwrap();
            }
            "missing" => std::fs::write(
                packages.join("bugs/worker.py"),
                "raise Exception('review unavailable')\n",
            )
            .unwrap(),
            "failed_checks" => {
                let path = packages.join("implementer/worker.py");
                let source = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("items[offset:offset+limit]", "items[:1]");
                std::fs::write(path, source).unwrap();
            }
            "missing_coverage" => {
                let path = packages.join("review/pipeline.toml");
                let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
                    review_config::task::parse_task_pipeline(
                        &std::fs::read_to_string(&path).unwrap(),
                    )
                    .unwrap();
                pipeline.coverage.clear();
                pipeline
                    .contract
                    .outputs
                    .get_mut("review")
                    .unwrap()
                    .covers
                    .clear();
                std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
            }
            "stale_checks" => {
                let path = packages.join("implementation/pipeline.toml");
                let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
                    review_config::task::parse_task_pipeline(
                        &std::fs::read_to_string(&path).unwrap(),
                    )
                    .unwrap();
                pipeline.nodes.push(serde_json::from_value(serde_json::json!({"id":"old_checks","operator":{"op":"check","checks":["pagination"]},"inputs":{"source":{"kind":"input","port":"source"}}})).unwrap());
                pipeline
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == "accept")
                    .unwrap()
                    .inputs
                    .insert(
                        "checks".into(),
                        serde_json::from_value(
                            serde_json::json!({"kind":"node","node":"old_checks","port":"result"}),
                        )
                        .unwrap(),
                    );
                std::fs::write(path, toml::to_string(&pipeline).unwrap()).unwrap();
            }
            _ => unreachable!(),
        }
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        for name in [
            "bugs",
            "correctness",
            "review",
            "implementation",
            "implementer",
        ] {
            let name_key = format!("fixture/{name}");
            catalog["packages"][&name_key]["digest"] = toml::Value::String(
                review_config::lock::package_digest(&name_key, &packages.join(name)).unwrap(),
            );
        }
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        for args in [["add", "-A"], ["commit", "-qm"]] {
            let mut command = Command::new("git");
            command.current_dir(&repo).args(args);
            if args[0] == "commit" {
                command.arg(case);
            }
            assert!(command.status().unwrap().success());
        }
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args([
                "task",
                "start",
                "--file",
                "ticket.json",
                "--json",
                "--state",
            ])
            .arg(&state)
            .output()
            .unwrap();
        let code = if matches!(case, "missing_coverage" | "stale_checks") {
            1
        } else if case == "missing" {
            4
        } else {
            3
        };
        assert_eq!(
            output.status.code(),
            Some(code),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if code != 1 {
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_ne!(result["result"]["acceptance"], "satisfied", "{case}");
            assert!(
                result["result"]["missing_obligations"]
                    .as_array()
                    .unwrap()
                    .contains(&Value::String("verified".into()))
            );
            assert_eq!(
                result["attempts"],
                if case == "failed_checks" { 2 } else { 4 }
            );
        }
    }
}

#[test]
fn implementation_embeds_the_same_locked_review_and_one_task_budget() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "embedded-review");
    let plan = af(&repo, &state, &["plan", "--file", "ticket.json"]);
    assert_eq!(
        plan["graph"]["calls"]["root.nodes.review"]["pipeline"],
        "fixture/review"
    );
    assert_eq!(plan["attempts"], 0);
    let result = af(&repo, &state, &["run", "pagination-cli"]);
    assert_eq!(
        result["attempts"], 4,
        "Implementation, checks, and two reviewers share four Attempts"
    );
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        result["result"]["outputs"]["verification"]["artifact_type"],
        "af/ReviewedImplementation@1"
    );
    let delivered = directory.path().join("delivered");
    af(
        &repo,
        &state,
        &[
            "deliver",
            "pagination-cli",
            "--confirm",
            "pagination-cli",
            "--branch",
            "reviewed-pagination",
            "--worktree",
            delivered.to_str().unwrap(),
        ],
    );
    let standalone_state = directory.path().join("standalone-state");
    let standalone_plan = af(
        &delivered,
        &standalone_state,
        &["plan", "--file", "review.json", "--uncommitted"],
    );
    assert_eq!(
        standalone_plan["plan"]["pipeline_id"],
        plan["plan"]["dependencies"]["fixture/review"]["artifact_id"]
    );
    let standalone = af(&delivered, &standalone_state, &["run", "standalone-cli"]);
    assert_eq!(standalone["attempts"], 3);
    assert_eq!(standalone["result"]["domain_conclusion"], "pass");
    let review = &result["review_rounds"][0];
    let alone = &standalone["review_rounds"][0];
    assert_eq!(review["outcome"], alone["outcome"]);
    assert_eq!(review["conclusion"], alone["conclusion"]);
    let captured = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let separate = review_store::Cas::open_existing(standalone_state.join("cas")).unwrap();
    for reviewer in ["bugs", "correctness"] {
        let embedded_result = captured
            .get_json(review["selected_results"][reviewer].as_str().unwrap())
            .unwrap();
        let standalone_result = separate
            .get_json(alone["selected_results"][reviewer].as_str().unwrap())
            .unwrap();
        assert_eq!(embedded_result["payload"], standalone_result["payload"]);
    }
}

#[test]
fn two_developers_share_one_pipeline_with_captured_private_worker_bindings() {
    use review_config::task::catalog::TaskWorkerManifest;
    let temp = tempfile::tempdir().unwrap();
    let (repo, _) = fixture(temp.path());
    let mut pipeline_ids = Vec::new();
    let mut worker_ids = Vec::new();
    for developer in ["alice", "bob"] {
        let local = temp.path().join(developer);
        let package = local.join("worker");
        copy_tree(
            &repo.join(".af/task-packages/fixture/implementer"),
            &package,
        );
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        worker.name = format!("local/{developer}");
        worker.signature.attempt.as_mut().unwrap().wall_ms =
            if developer == "alice" { 4500 } else { 4800 };
        std::fs::write(
            package.join("worker.toml"),
            toml::to_string(&worker).unwrap(),
        )
        .unwrap();
        let digest = review_config::lock::package_digest(&worker.name, &package).unwrap();
        let bindings = local.join("bindings.toml");
        std::fs::write(&bindings, toml::to_string(&serde_json::json!({
            "schema":"af.task-bindings/1", "packages":{worker.name.clone():{"version":"1.0.0", "digest":digest,"path":"worker"}},
            "slots":{"root.slots.implementer":worker.name}
        })).unwrap()).unwrap();
        let state = temp.path().join(format!("state-{developer}"));
        let plan = af(
            &repo,
            &state,
            &[
                "plan",
                "--file",
                "ticket.json",
                "--bindings",
                bindings.to_str().unwrap(),
            ],
        );
        pipeline_ids.push(plan["plan"]["pipeline_id"].clone());
        worker_ids
            .push(plan["plan"]["bindings"]["root.slots.implementer"]["package_digest"].clone());
        assert_eq!(plan["attempts"], 0);
        assert_eq!(
            plan["graph"]["slots"]["root.slots.implementer"]["worker"],
            format!("local/{developer}")
        );
        std::fs::write(
            package.join("worker.py"),
            "raise Exception('changed local package must never execute')",
        )
        .unwrap();
        std::fs::write(&bindings, "invalid after capture").unwrap();
        let run = af(&repo, &state, &["run", "pagination-cli"]);
        assert_eq!(run["plan_id"], plan["plan_id"]);
        assert_eq!(run["result"]["acceptance"], "satisfied");
        assert_eq!(run["attempts"], 3);
    }
    assert_eq!(pipeline_ids[0], pipeline_ids[1]);
    assert_ne!(worker_ids[0], worker_ids[1]);
}

#[test]
fn plan_then_run_uses_captured_inputs_and_does_not_repeat_finished_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    let planned = af(&repo, &state, &["plan", "--file", "ticket.json"]);
    assert_eq!(planned["schema"], "af/task-inspection@2");
    assert_eq!(planned["attempts"], 0);
    assert!(planned["plan"].is_object());
    assert!(planned["graph"].is_object());
    assert!(state.join("events.sqlite").is_file());
    assert!(!state.join("tasks.sqlite").exists());

    std::fs::write(repo.join("pagination.py"), "live source changed\n").unwrap();
    std::fs::write(repo.join(".af/code-policy.toml"), "invalid after planning").unwrap();
    std::fs::write(
        repo.join(".af/task-packages/fixture/implementer/worker.py"),
        "raise Exception('live Worker must not run')",
    )
    .unwrap();
    let run = af(&repo, &state, &["run", "pagination-cli"]);
    assert_eq!(run["attempts"], 3);
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["plan_id"], planned["plan_id"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("pagination.py")).unwrap(),
        "live source changed\n"
    );
    let resumed = af(&repo, &state, &["run", "pagination-cli"]);
    assert_eq!(resumed, run);
    let explained = af(&repo, &state, &["explain", "pagination-cli"]);
    assert_eq!(explained["plan"], planned["plan"]);
    assert_eq!(explained["graph"], planned["graph"]);
    let shown = af(&repo, &state, &["show", "pagination-cli"]);
    assert_eq!(shown, run);
    let listed = af(&repo, &state, &["list"]);
    assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(listed["tasks"][0]["task_id"], "pagination-cli");
}

#[test]
fn task_start_accepts_a_file_without_legacy_goal_or_kind_flags() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    let run = af(&repo, &state, &["start", "--file", "ticket.json"]);
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["attempts"], 3);
    assert!(!state.join("tasks.sqlite").exists());
    let destination = temp.path().join("delivered");
    let deliver = [
        "deliver",
        "pagination-cli",
        "--branch",
        "task/pagination",
        "--worktree",
        destination.to_str().unwrap(),
        "--confirm",
        "pagination-cli",
    ];
    let receipt = af(&repo, &state, &deliver);
    assert_eq!(receipt["outcome"]["kind"], "delivered");
    assert_eq!(receipt["remote_actions"], serde_json::json!([]));
    assert_eq!(af(&repo, &state, &deliver), receipt);
    assert!(
        std::fs::read_to_string(destination.join("pagination.py"))
            .unwrap()
            .contains("offset")
    );
    assert!(!state.join("tasks.sqlite").exists());
}
