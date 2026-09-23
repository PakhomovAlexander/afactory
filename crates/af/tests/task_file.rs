//! Public Task-file path: captured authority survives CLI process boundaries and edits.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

#[path = "task_file/wall_bounds.rs"]
mod wall_bounds;

#[path = "support/task_cli.rs"]
mod task_cli;
use task_cli::{copy_tree, fixture_named};

/// Native-model fixtures plan, reject one changed account, restore it, then resume the same Task.
/// Keep that absolute deadline away from loaded-gate latency without changing any dispatch or
/// verification bound (ADR-0113).
const NATIVE_MODEL_TASK_WALL_MS: u64 = 600_000;

fn native_model_limits() -> Value {
    serde_json::json!({
        "tokens": 10_000,
        "max_attempts": 4,
        "wall_ms": NATIVE_MODEL_TASK_WALL_MS,
        "verification": {"tokens": 6_096, "attempts": 4, "wall_ms": 60_000}
    })
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    fixture_named(root, "pagination")
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
    let replay = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "run",
            "--execute",
            "review-cli",
            "--json",
            "--state",
        ])
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

#[test]
fn native_model_cli_admission_is_shared_and_account_changes_refuse_dispatch() {
    native_model_case(false, false);
}

#[test]
fn native_model_cli_retains_wide_failed_usage_in_json_and_text_inspection() {
    native_model_case(true, false);
}

#[test]
fn native_codex_multiturn_usage_survives_common_accounting_and_fresh_inspection() {
    native_model_case(true, true);
}

fn native_model_case(wide: bool, codex: bool) {
    native_model_drift_case(wide, codex, None);
}

#[test]
fn native_task_account_change_after_admission_refuses_private_worker_context() {
    native_model_drift_case(false, false, Some(1));
}
#[test]
fn native_task_account_change_between_workers_retains_original_spend() {
    native_model_drift_case(false, false, Some(2));
}

#[test]
fn native_model_fixture_widens_only_its_total_task_wall() {
    let limits = native_model_limits();
    assert_eq!(limits["wall_ms"], NATIVE_MODEL_TASK_WALL_MS);
    assert_eq!(limits["tokens"], 10_000);
    assert_eq!(limits["max_attempts"], 4);
    assert_eq!(limits["verification"]["tokens"], 6_096);
    assert_eq!(limits["verification"]["attempts"], 4);
    assert_eq!(limits["verification"]["wall_ms"], 60_000);
}

fn native_model_drift_case(wide: bool, codex: bool, switch_after: Option<usize>) {
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
    if let Some(after) = switch_after {
        std::fs::write(home.join("switch-after"), after.to_string()).unwrap();
    }
    if wide {
        std::fs::write(home.join("wide-usage"), b"fixture").unwrap();
    }
    let stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CLAUDE_CONFIG_DIR']
if sys.argv[1:3]==['auth','status']:
 print(json.dumps({'loggedIn':True,'apiProvider':'firstParty','authMethod':'claude.ai','email':open(home+'/account-email').read()}))
 sys.exit(0)
request=sys.stdin.read()
with open(home+'/calls','a') as f: f.write(open(home+'/account-email').read()+'\n')
if os.path.isfile(home+'/switch-after') and len(open(home+'/calls').readlines())==int(open(home+'/switch-after').read()):
 with open(home+'/account-email','w') as f: f.write('changed@example.test')
if os.path.isfile(home+'/wide-usage'):
 print(json.dumps({'is_error':True,'result':'fixture provider failed after reporting usage','usage':{'input_tokens':18446744073709551615,'output_tokens':20,'cache_creation_input_tokens':0}}))
 sys.exit(0)
if request=='Reply with exactly: OK\n':
 result='OK'
else:
 value=json.loads(request)
 assert set(value['inputs'])=={'source','subject','history','checks','assignment'}
 assert value['inputs']['assignment'][0]['payload']['findings']==[]
 result=json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}})
envelope={'is_error':False,'result':result,'usage':{'input_tokens':10,'output_tokens':2,'cache_creation_input_tokens':0}}
if request!='Reply with exactly: OK\n':
 assert '--json-schema' in sys.argv
 envelope['structured_output']=json.loads(result)
print(json.dumps(envelope))
"#;
    let codex_stub = r#"#!/usr/bin/python3
import os,json,sys
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':open(home+'/account-email').read()}}}),flush=True)
 sys.exit(0)
sys.stdin.read()
with open(home+'/calls','a') as f: f.write('model\n')
for i,c,o,r,w in [(18446744073709551615,7,18446744073709551615,18446744073709551615,18446744073709551615),(20,3,30,40,50)]:
 print(json.dumps({'type':'turn.completed','usage':{'input_tokens':i,'cached_input_tokens':c,'output_tokens':o,'reasoning_output_tokens':r,'cache_write_input_tokens':w}}))
print(json.dumps({'type':'turn.failed','error':{'message':'fixture failed after two paid turns'}}))
"#;
    let kind = if codex { "codex" } else { "claude" };
    let provider = format!("{kind}-personal");
    std::fs::write(bin.join(kind), if codex { codex_stub } else { stub }).unwrap();
    std::fs::set_permissions(bin.join(kind), std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = home.join("providers.toml");
    std::fs::write(&registry,toml::to_string(&serde_json::json!({"version":1,"providers":[{"id":provider,"kind":kind,"auth_dir":home}]})).unwrap()).unwrap();
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
            provider_kind: kind.into(),
            model: format!("{kind}-fixture-1"),
            effort: "high".into(),
        };
        worker.signature.attempt.as_mut().unwrap().tokens = 1000;
        std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
        catalog["packages"][&package]["digest"] =
            toml::Value::String(review_config::lock::package_digest(&package, &path).unwrap());
        providers.insert(package, toml::Value::String(provider.clone()));
    }
    let pipeline_dir = repo.join(".af/task-packages/fixture/review");
    let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
            .unwrap();
    pipeline.max_attempts = 4;
    if switch_after.is_some() {
        pipeline.max_parallel = 1;
    }
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
    file["limits"] = native_model_limits();
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
        "{}\n{}",
        String::from_utf8_lossy(&plan.stdout),
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
    let changed = run(&["task", "run", "--execute", "review-cli"]);
    assert!(!changed.status.success());
    assert!(!calls.exists(), "Changed account reached model dispatch");
    std::fs::write(&email, "developer@example.test").unwrap();
    let output = run(&["task", "run", "--execute", "review-cli"]);
    if let Some(after) = switch_after {
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["chargeable_tokens"], (12 * after).to_string());
        assert_ne!(result["result"]["domain_conclusion"], "pass");
        let sent = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(sent.lines().count(), after);
        assert!(
            sent.lines().all(|line| line == "developer@example.test"),
            "private input reached another account"
        );
        let records = result["execution_records"].as_array().unwrap();
        assert!(
            records
                .iter()
                .any(|r| r["record"]["kind"] == "settled" && r["record"]["charged_tokens"] == "0"),
            "pre-send refusal must be known zero: {records:?}"
        );
        let shown = run(&["task", "show", "review-cli"]);
        assert!(shown.status.success());
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["chargeable_tokens"], result["chargeable_tokens"]);
        assert_eq!(shown["attempts"], result["attempts"]);
        assert_eq!(std::fs::read_to_string(&calls).unwrap(), sent);
        let cas = review_store::Cas::open(state.join("cas")).unwrap();
        let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let projection = store.task_projection(&cas, "review-cli").unwrap().unwrap();
        assert_eq!(projection.revision.limits.tokens, 10000);
        let execution = projection.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), (12 * after) as u128);
        assert!(
            execution.outputs.contains_key("root.providers.admit0"),
            "succeeded admission was lost"
        );
        assert!(execution.budget.begun_attempts() <= 4);
        return;
    }
    if wide {
        let exact = if codex {
            2 * u128::from(u64::MAX) + 40
        } else {
            u128::from(u64::MAX) + 20
        };
        assert_eq!(
            output.status.code(),
            Some(4),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["schema"], "af/task-inspection@11");
        assert_eq!(result["chargeable_tokens"], exact.to_string());
        assert_eq!(result["result"]["domain_conclusion"], "incomplete");
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        let shown = run(&["task", "show", "review-cli"]);
        assert!(shown.status.success());
        let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["chargeable_tokens"], result["chargeable_tokens"]);
        let listed = run(&["task", "list"]);
        assert!(listed.status.success());
        let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert_eq!(listed["schema"], "af/task-list@2");
        assert_eq!(listed["tasks"][0]["schema"], "af/task-list-entry@2");
        assert_eq!(listed["tasks"][0]["chargeable_tokens"], exact.to_string());
        let text = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args(["task", "list", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(text.status.success());
        assert!(String::from_utf8_lossy(&text.stdout).contains(&format!("{exact} tokens")));
        let cas = review_store::Cas::open(state.join("cas")).unwrap();
        let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let projection = store.task_projection(&cas, "review-cli").unwrap().unwrap();
        let execution = projection.execution.unwrap();
        assert_eq!(execution.budget.committed_tokens(), exact);
        assert_eq!(execution.budget.begun_attempts(), 1);
        assert_eq!(projection.revision.limits.tokens, 10_000);
        let walls = store
            .task_attempt_wall(&review_store::store::task::task_run_id("review-cli").unwrap())
            .unwrap();
        let usage = walls[0].usage.as_ref().unwrap();
        assert_eq!(usage.chargeable_tokens.get(), exact);
        if codex {
            assert_eq!(usage.input_tokens.unwrap().get(), u128::from(u64::MAX) + 20);
            assert_eq!(
                usage.output_tokens.unwrap().get(),
                u128::from(u64::MAX) + 30
            );
        }
        let settled = result["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["record"]["kind"] == "settled")
            .unwrap();
        let id = settled["record"]["usage_id"].as_str().unwrap();
        assert_eq!(
            cas.get_artifact(id).unwrap().artifact_type,
            review_core::task::usage::TASK_TOKEN_USAGE_V3
        );
        assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 1);
        return;
    }
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["attempts"], 4);
    assert_eq!(result["chargeable_tokens"], "36");
    assert_eq!(result["result"]["domain_conclusion"], "pass");
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 3);
    assert!(
        run(&["task", "run", "--execute", "review-cli"])
            .status
            .success()
    );
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
fn captured_task_kind_packages_keep_business_names_and_domain_acceptance() {
    for (fixture_name, file_name, profile, kind, root, exit) in [
        (
            "embedded-review",
            "ticket.json",
            "reviewed_implementation",
            "team/feature",
            "implementation",
            0,
        ),
        (
            "review",
            "review.json",
            "review",
            "team/security-review",
            "review",
            3,
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), fixture_name);
        let package_name = "team/task-kind";
        let package = repo.join(".af/task-packages/team/task-kind");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("kind.toml"), toml::to_string(&serde_json::json!({"schema":"af.task-kind/1","name":package_name,"version":"1.0.0","kind":kind,"profile":profile})).unwrap()).unwrap();
        let file = repo.join(file_name);
        let mut ticket: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        ticket["kind"] = Value::String(kind.into());
        ticket.as_object_mut().unwrap().remove("verification");
        std::fs::write(&file, serde_json::to_vec(&ticket).unwrap()).unwrap();
        let pipeline_dir = repo.join(format!(".af/task-packages/fixture/{root}"));
        let mut pipeline = review_config::task::parse_task_pipeline(
            &std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap(),
        )
        .unwrap();
        pipeline.accepts.kinds = std::collections::BTreeSet::from([kind.into()]);
        std::fs::write(
            pipeline_dir.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        catalog["packages"].as_table_mut().unwrap().insert(package_name.into(), toml::Value::try_from(serde_json::json!({"version":"1.0.0","digest":review_config::lock::package_digest(package_name,&package).unwrap(),"path":".af/task-packages/team/task-kind"})).unwrap());
        catalog["packages"][format!("fixture/{root}")]["digest"] = toml::Value::String(
            review_config::lock::package_digest(&format!("fixture/{root}"), &pipeline_dir).unwrap(),
        );
        catalog.as_table_mut().unwrap().insert(
            "kinds".into(),
            toml::Value::try_from(serde_json::json!({kind:package_name})).unwrap(),
        );
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "kind package"]] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let plan = af(&repo, &state, &["plan", "--file", file_name]);
        assert!(plan["plan"]["dependencies"][package_name].is_object());
        let task_id = ticket["task_id"].as_str().unwrap();
        std::fs::write(package.join("kind.toml"), "modified after capture").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args(["task", "run", "--execute", task_id, "--json", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["result"]["acceptance"], "satisfied");
        assert_eq!(result["plan_id"], plan["plan_id"]);
    }
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
                let reply = serde_json::json!({"schema":"af.worker-reply/1","outputs":{"result":[{"reports":[{"severity":"major","file":"pagination.py","line":1,"title":"Missing validation","body":"Offset must reject negative values","fix":"Validate offset","confidence":0.9}],"benchmark_demands":[],"dispositions":[]}]}});
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
                "--execute",
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
                if case == "failed_checks" { 2 } else { 5 }
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
    let result = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
    assert_eq!(
        result["attempts"], 5,
        "Implementation, checks, two reviewers and goal evaluation share five Attempts"
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
    let standalone = af(
        &delivered,
        &standalone_state,
        &["run", "--execute", "standalone-cli"],
    );
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
        let revision = captured
            .get_json(result["revision_id"].as_str().unwrap())
            .unwrap();
        let requirement = &revision["payload"]["inputs"]["requirements"]["artifact_ids"][0];
        assert!(
            embedded_result["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        assert!(
            !standalone_result["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        let verified = captured
            .get_json(
                result["result"]["outputs"]["evaluation"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let evaluation = captured
            .get_json(verified["payload"]["evaluation_id"].as_str().unwrap())
            .unwrap();
        assert!(
            evaluation["input_artifacts"]
                .as_array()
                .unwrap()
                .contains(requirement)
        );
        assert_eq!(
            evaluation["subject_snapshot_id"],
            result["result"]["outputs"]["snapshot"]["snapshot_id"]
        );
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
        let run = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
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
    assert_eq!(planned["schema"], "af/task-inspection@11");
    assert_eq!(planned["attempts"], 0);
    assert!(planned["plan"].is_object());
    assert!(planned["graph"].is_object());
    assert!(state.join("events.sqlite").is_file());

    std::fs::write(repo.join("pagination.py"), "live source changed\n").unwrap();
    std::fs::write(repo.join(".af/code-policy.toml"), "invalid after planning").unwrap();
    std::fs::write(
        repo.join(".af/task-packages/fixture/implementer/worker.py"),
        "raise Exception('live Worker must not run')",
    )
    .unwrap();
    let run = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
    assert_eq!(run["attempts"], 3);
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["plan_id"], planned["plan_id"]);
    assert_eq!(
        std::fs::read_to_string(repo.join("pagination.py")).unwrap(),
        "live source changed\n"
    );
    let resumed = af(&repo, &state, &["run", "--execute", "pagination-cli"]);
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
fn task_start_runs_a_task_file_and_requires_one() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = fixture(temp.path());
    // `--file` is the only way to describe a Task; the fixed-format flags are gone.
    for args in [
        "start --execute",
        "start --kind implement --goal pagination",
        "start --file ticket.json --pipeline .af/pipelines/implement.toml",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .arg("task")
            .args(args.split(' '))
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "af task {args}");
    }
    assert!(!state.exists());
    let run = af(
        &repo,
        &state,
        &["start", "--execute", "--file", "ticket.json"],
    );
    assert_eq!(run["result"]["acceptance"], "satisfied");
    assert_eq!(run["attempts"], 3);
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
}

#[test]
fn current_review_catalog_uses_exact_assignments_and_reopens_without_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, state) = fixture_named(directory.path(), "review-v2");
    let invoke = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args(args)
            .args(["--json", "--state"])
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
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let result = invoke(&["review", "run", "--file", "review.json"]);
    assert_eq!(result["attempts"], 6);
    assert_eq!(result["result"]["execution"], "completed");
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        result["result"]["domain_conclusion"],
        "convergence_exhausted"
    );
    let explained = invoke(&["task", "explain", "review-cli"]);
    let graph = serde_json::to_string(&explained["graph"]).unwrap();
    assert!(graph.contains("af/TaskReviewSubject@2"));
    assert!(graph.contains("af/TaskReviewAssignment@1"));
    assert!(graph.contains("review.kernel/ReviewerResult@2"));
    assert_eq!(invoke(&["task", "run", "--execute", "review-cli"]), result);
}
