//! Real CLI/Store lifecycle with a local deterministic Codex protocol substitute. No credentials,
//! network or model inference are used. Synthetic usage reproduces the admission sizing boundary.
use super::*;
use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    path: std::ffi::OsString,
    catalog: Value,
}

impl Fixture {
    fn new(version: u8, actual: u64, tokens: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = setup(root.path());
        let home = root.path().join("home");
        let bin = root.path().join("bin");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let native = r#"#!/usr/bin/python3 -B
import json,os,sys
home=os.environ['CODEX_HOME']
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  request=json.loads(line)
  if request.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if request.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':'fixture@example.invalid'}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
if request=='Reply with exactly: OK\n':
 kind='admission'; message='OK'; usage=json.load(open(home+'/usage.json'))
else:
 kind='author'; r=json.loads(request)
 assert set(r['inputs'])=={'requirements','sources'}
 s=r['inputs']['sources'][0]['payload']['sources']
 message=json.dumps({'schema':'af.worker-reply/1','outputs':{'draft':[{'schema':'af.document-draft/1','title':'Release notes','sections':[{'heading':'Summary','body':'\n\n'.join(s[k]['text'] for k in sorted(s))}],'citations':sorted(s)}]}})
 usage={'input_tokens':3,'cached_input_tokens':0,'output_tokens':2,'reasoning_output_tokens':0,'cache_write_input_tokens':0}
with open(home+'/calls','a') as f: f.write(kind+'\n')
print(json.dumps({'type':'thread.started','thread_id':'synthetic-'+kind}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':usage}))
"#;
        std::fs::write(bin.join("codex"), native).unwrap();
        std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let usage = match actual {
            5712 => {
                json!({"input_tokens":16331,"cached_input_tokens":10624,"output_tokens":5,"reasoning_output_tokens":0,"cache_write_input_tokens":0})
            }
            16336 => {
                json!({"input_tokens":16331,"cached_input_tokens":0,"output_tokens":5,"reasoning_output_tokens":0,"cache_write_input_tokens":0})
            }
            _ => {
                json!({"input_tokens":actual,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"cache_write_input_tokens":0})
            }
        };
        std::fs::write(home.join("usage.json"), serde_json::to_vec(&usage).unwrap()).unwrap();
        std::fs::write(home.join("providers.toml"), toml::to_string(&json!({"version":1,"providers":[{"id":"codex-personal","kind":"codex","auth_dir":home}]})).unwrap()).unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        catalog["schema"] = json!(format!("af.task-catalog/{version}"));
        if version == 2 {
            catalog["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
        }
        catalog["providers"] = json!({"builtin/document-author":"codex-personal"});
        let package = repo.join(
            catalog["packages"]["builtin/document-author"]["path"]
                .as_str()
                .unwrap(),
        );
        let mut worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        worker.runner = TaskWorkerRunner::Model {
            provider_kind: "codex".into(),
            model: "codex-fixture-1".into(),
            effort: "high".into(),
        };
        worker.signature.attempt.as_mut().unwrap().tokens = 16384;
        worker.signature.attempt.as_mut().unwrap().wall_ms = 180000;
        std::fs::write(
            package.join("worker.toml"),
            toml::to_string(&worker).unwrap(),
        )
        .unwrap();
        catalog["packages"]["builtin/document-author"]["digest"] = json!(
            review_config::lock::package_digest("builtin/document-author", &package).unwrap()
        );
        let package = repo.join(
            catalog["packages"]["builtin/release-notes"]["path"]
                .as_str()
                .unwrap(),
        );
        let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
            toml::from_str(&std::fs::read_to_string(package.join("pipeline.toml")).unwrap())
                .unwrap();
        pipeline.max_attempts = 4;
        std::fs::write(
            package.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        catalog["packages"]["builtin/release-notes"]["digest"] =
            json!(review_config::lock::package_digest("builtin/release-notes", &package).unwrap());
        std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let task_path = repo.join("document.json");
        let mut task: Value = serde_json::from_slice(&std::fs::read(&task_path).unwrap()).unwrap();
        task["limits"] = json!({"tokens":tokens,"max_attempts":4,"wall_ms":300000,"verification":{"tokens":0,"attempts":2,"wall_ms":10000}});
        std::fs::write(task_path, serde_json::to_vec(&task).unwrap()).unwrap();
        commit(&repo);
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            _root: root,
            repo,
            state,
            home,
            path,
            catalog,
        }
    }

    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "fixture")
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.home.join("providers.toml"))
            .args(args)
            .args(["--json", "--state"])
            .arg(&self.state)
            .output()
            .unwrap()
    }
}

#[test]
fn captured_admission_cost_survives_checkout_changes_and_fresh_store_replay() {
    for (version, actual, accepted) in [
        (1, 5712, false),
        (2, 5712, true),
        (2, 16336, true),
        (2, 32769, false),
    ] {
        // V1 deliberately has the same ample total as V2: its own reservation still fences it.
        let mut f = Fixture::new(version, actual, 49152);
        let planned = success(f.cli(&["task", "plan", "--file", "document.json"]));
        assert!(
            !f.home.join("calls").exists(),
            "planning ran the synthetic capability call"
        );
        let allowance = if version == 1 { 4096 } else { 32768 };
        assert_eq!(
            planned["graph"]["allowances"]["root.providers.admit0"],
            json!({"tokens_per_attempt":allowance,"wall_ms_per_attempt":45000,"max_attempts":1,"verification_attempts":0})
        );
        let cas = review_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let authority = cas
            .get_json(planned["plan"]["authority"]["policy_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            authority["schema"],
            format!("af.task-run-authority/{version}")
        );
        if version == 1 {
            assert!(
                !authority
                    .as_object()
                    .unwrap()
                    .contains_key("provider_admission")
            );
        } else {
            assert_eq!(
                authority["provider_admission"],
                json!({"tokens":32768,"wall_ms":45000})
            );
        }
        let captured_bytes = cas.get(authority["catalog_id"].as_str().unwrap()).unwrap();
        assert_eq!(
            captured_bytes,
            std::fs::read(f.repo.join(".af/task-catalog.toml")).unwrap()
        );
        // Change only the disposable fixture checkout after capture. Run must ignore new policy,
        // Task-file resources and source text, and use the original CAS authority instead.
        f.catalog["schema"] = json!("af.task-catalog/2");
        f.catalog["provider_admission"] = json!({"tokens":1,"wall_ms":1});
        std::fs::write(
            f.repo.join(".af/task-catalog.toml"),
            toml::to_string(&f.catalog).unwrap(),
        )
        .unwrap();
        std::fs::write(f.repo.join("document.json"), b"changed input and limits").unwrap();
        std::fs::write(f.repo.join("sources.json"), b"changed sources").unwrap();
        commit(&f.repo);
        let out = f.cli(&["task", "run", "--execute", "release-notes"]);
        assert_eq!(
            out.status.code(),
            Some(if accepted { 0 } else { 4 }),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let done: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(done["plan_id"], planned["plan_id"]);
        assert_eq!(done["revision_id"], planned["revision_id"]);
        assert_eq!(done["attempts"], if accepted { 4 } else { 1 });
        assert_eq!(
            done["chargeable_tokens"],
            (actual + if accepted { 5 } else { 0 }).to_string()
        );
        assert_eq!(
            done["result"]["execution"],
            if accepted { "completed" } else { "exhausted" }
        );
        assert_eq!(
            done["result"]["acceptance"],
            if accepted {
                "satisfied"
            } else {
                "inconclusive"
            }
        );
        let calls = std::fs::read_to_string(f.home.join("calls")).unwrap();
        assert_eq!(
            calls,
            if accepted {
                "admission\nauthor\n"
            } else {
                "admission\n"
            }
        );
        let store =
            review_store::EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
        let projection = store
            .task_projection(&cas, "release-notes")
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&projection.revision.limits).unwrap(),
            planned["plan"]["limits"]
        );
        let execution = projection.execution.unwrap();
        assert_eq!(execution.budget.breached(), !accepted);
        assert_eq!(
            execution.budget.committed_tokens(),
            u128::from(actual + if accepted { 5 } else { 0 })
        );
        let attempts = execution.attempt_accounting();
        let admission = attempts
            .iter()
            .find(|a| a.reservation.node == "root.providers.admit0")
            .unwrap();
        assert_eq!(admission.reservation.tokens, allowance);
        assert_eq!(admission.charged_tokens, u128::from(actual));
        assert!(execution.outputs.contains_key("root.providers.admit0"));
        assert_eq!(
            attempts
                .iter()
                .any(|a| a.reservation.node == "root.nodes.author" && a.started),
            accepted
        );
        drop(store);
        let replay = f.cli(&["task", "run", "--execute", "release-notes"]);
        assert_eq!(
            serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
            done
        );
        assert_eq!(
            std::fs::read_to_string(f.home.join("calls")).unwrap(),
            calls
        );
    }
}

#[test]
fn v2_catalog_schema_and_cli_refuse_malformed_and_underfunded_admission() {
    let mut f = Fixture::new(2, 5712, 49151);
    let schema: Value =
        serde_json::from_str(include_str!("../../../../schemas/task-catalog-v2.json")).unwrap();
    let validator = catalog_schema(&schema);
    assert!(validator.is_valid(&f.catalog));
    let old: Value =
        serde_json::from_str(include_str!("../../../../schemas/task-catalog-v1.json")).unwrap();
    assert!(!catalog_schema(&old).is_valid(&f.catalog));
    let out = f.cli(&["task", "plan", "--file", "document.json"]);
    assert!(
        !out.status.success(),
        "underfunded admission and author were accepted"
    );
    let refusal: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        refusal["selection"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| {
                candidate["state"]["kind"] == "infeasible"
                    && candidate["state"]["reason"]
                        .as_str()
                        .unwrap()
                        .contains("token and Attempt allowance")
            })
    );
    assert!(!f.home.join("calls").exists());
    for cost in [
        Value::Null,
        json!({"tokens":0,"wall_ms":45000}),
        json!({"tokens":32768,"wall_ms":0}),
        json!({"tokens":9007199254740992_u64,"wall_ms":45000}),
        json!({"tokens":32768,"wall_ms":9007199254740992_u64}),
        json!({"tokens":32768}),
        json!({"tokens":32768,"wall_ms":45000,"extra":1}),
    ] {
        f.catalog["provider_admission"] = cost;
        assert!(!validator.is_valid(&f.catalog));
    }
    // Exercise the actual configuration parser once; remaining shape boundaries have both
    // Rust unit and schema controls. No account or native capability operation is necessary.
    f.catalog["schema"] = json!("af.task-catalog/1");
    f.catalog["provider_admission"] = json!({"tokens":32768,"wall_ms":45000});
    std::fs::write(
        f.repo.join(".af/task-catalog.toml"),
        toml::to_string(&f.catalog).unwrap(),
    )
    .unwrap();
    commit(&f.repo);
    let out = f.cli(&["task", "plan", "--file", "document.json"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("V1 forbids provider_admission"));
    assert!(!f.home.join("calls").exists());
}

#[test]
fn admission_tokens_and_wall_exceeding_original_task_limits_refuse_paid_dispatch() {
    for (cost, reason) in [
        (
            json!({"tokens":49153,"wall_ms":45000}),
            "token and Attempt allowance",
        ),
        (
            json!({"tokens":32768,"wall_ms":300001}),
            "Remaining Task deadline",
        ),
    ] {
        let mut f = Fixture::new(2, 5712, 49152);
        f.catalog["provider_admission"] = cost;
        std::fs::write(
            f.repo.join(".af/task-catalog.toml"),
            toml::to_string(&f.catalog).unwrap(),
        )
        .unwrap();
        commit(&f.repo);
        let out = f.cli(&["task", "plan", "--file", "document.json"]);
        assert!(!out.status.success());
        let refusal: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(refusal["attempts"], 0);
        assert!(
            refusal["selection"]["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| {
                    candidate["state"]["kind"] == "infeasible"
                        && candidate["state"]["reason"]
                            .as_str()
                            .unwrap()
                            .contains(reason)
                }),
            "{refusal}"
        );
        // The existing token-free native account protocol may run during binding. The paid
        // capability protocol and author are never invoked when original resources cannot fit.
        assert!(!f.home.join("calls").exists());
    }
}

fn catalog_schema(schema: &Value) -> jsonschema::Validator {
    let contracts: Value =
        serde_json::from_str(include_str!("../../../../schemas/task-contracts-v1.json")).unwrap();
    let planner: Value = serde_json::from_str(include_str!(
        "../../../../schemas/task-planner-settings-v1.json"
    ))
    .unwrap();
    let registry = jsonschema::Registry::new()
        .add(
            "urn:af:schema:task-contracts:1",
            jsonschema::Resource::from_contents(contracts),
        )
        .unwrap()
        .add(
            "urn:af:schema:task-planner-settings:1",
            jsonschema::Resource::from_contents(planner),
        )
        .unwrap()
        .prepare()
        .unwrap();
    jsonschema::options()
        .with_registry(&registry)
        .build(schema)
        .unwrap()
}
