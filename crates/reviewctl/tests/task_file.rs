//! Public Task-file path: captured authority survives CLI process boundaries and edits.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[path = "task_file/wall_bounds.rs"]
mod wall_bounds;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            // Gates supply read-only source files. These are independent disposable
            // fixtures whose tests intentionally edit them; keep the source immutable.
            let mut permissions = std::fs::metadata(&target).unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(false);
            std::fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn copied_read_only_files_allow_owner_mutation_without_changing_source_or_execution_bits() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let copy = root.path().join("copy");
    std::fs::create_dir(&source).unwrap();
    let files = [
        ("worker.py", 0o444),
        ("executable", 0o555),
        ("private", 0o440),
    ];
    for (name, mode) in files {
        let path = source.join(name);
        std::fs::write(&path, b"original fixture\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    copy_tree(&source, &copy);

    for (name, mode) in files {
        let path = copy.join(name);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            mode | 0o200,
            "only the disposable copy's owner-write bit may be added"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original fixture\n");
        std::fs::write(path, b"changed fixture\n").unwrap();
        let original = source.join(name);
        assert_eq!(std::fs::read(&original).unwrap(), b"original fixture\n");
        assert_eq!(
            std::fs::metadata(original).unwrap().permissions().mode() & 0o777,
            mode
        );
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

#[test]
fn task_file_legacy_workers_receive_bound_metadata_with_zero_command_reservations() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for (name, protocol, budget, body) in [
        (
            "implementer",
            "implement_v1",
            777,
            "open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\nprint('Implemented pagination')\n",
        ),
        (
            "evaluator",
            "evaluate_v1",
            0,
            "import runpy\nassert request['gates'][0]['status']=='passed'\nassert runpy.run_path('pagination.py')['paginate'](list(range(7)),2,3)==[2,3,4]\nprint(json.dumps({'verdict':'approve','summary':'Verified offset and limit'}))\n",
        ),
    ] {
        let package = repo.join(format!(".af/task-packages/fixture/{name}"));
        let path = package.join("worker.toml");
        let mut worker: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        worker["runner"]["kind"] = toml::Value::String("legacy_task_command".into());
        worker["runner"]
            .as_table_mut()
            .unwrap()
            .insert("protocol".into(), toml::Value::String(protocol.into()));
        worker["runner"]
            .as_table_mut()
            .unwrap()
            .insert("legacy_budget_tokens".into(), toml::Value::Integer(budget));
        std::fs::write(path, toml::to_string(&worker).unwrap()).unwrap();
        std::fs::write(package.join("worker.py"), format!("import json,sys\nrequest=json.loads(sys.stdin.read().split('```json\\n',1)[1].split('\\n```',1)[0])\nassert request['task_id']=='pagination-cli'\nassert request['budget']=={{'reserved_tokens':{budget}}}\n{body}")).unwrap();
        fn package_files(
            root: &Path,
            current: &Path,
            result: &mut std::collections::BTreeMap<String, Vec<u8>>,
        ) {
            for entry in std::fs::read_dir(current).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    package_files(root, &entry.path(), result);
                } else {
                    result.insert(
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .replace('\\', "/"),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        package_files(&package, &package, &mut files);
        catalog["packages"][format!("fixture/{name}")]["digest"] =
            toml::Value::String(review_config::lock::package_digest_from_files(&files));
    }
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [["add", "-A"], ["commit", "-qm"]] {
        let mut cmd = Command::new("git");
        cmd.current_dir(&repo).args(args);
        if args[0] == "commit" {
            cmd.arg("explicit legacy fixture");
        }
        assert!(cmd.status().unwrap().success());
    }
    let run = af(&repo, &state, &["start", "--file", "ticket.json"]);
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["attempts"], 3);
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task = store
        .task_projection(&cas, "pagination-cli")
        .unwrap()
        .unwrap();
    let requirements = &task.revision.inputs["requirements"].artifact_ids[0];
    let payload = cas.get_json(requirements).unwrap()["payload"].clone();
    assert_eq!(
        payload.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["text"]
    );
    let mut workers = 0;
    for entry in run["execution_records"].as_array().unwrap() {
        let record = &entry["record"];
        if record["kind"] != "prepared" {
            continue;
        }
        assert_eq!(record["reserved_tokens"], 0);
        let context = cas
            .get_json(record["context_id"].as_str().unwrap())
            .unwrap();
        if context["type"] == "af/TaskContext@2" {
            workers += 1;
            assert_eq!(context["payload"]["legacy"]["task_id"], "pagination-cli");
            assert_eq!(context["payload"]["legacy"]["plan_id"], run["plan_id"]);
        }
    }
    assert_eq!(workers, 2);
    assert_eq!(af(&repo, &state, &["run", "pagination-cli"]), run);
}
