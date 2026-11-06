//! Custom data Tasks through capture, the standard model adapter, durable acceptance and Scenario projection.
use crate::task_report::{git, text};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    auth: PathBuf,
    path: std::ffi::OsString,
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = crate::task_cli::fixture_named(root.path(), "acceptance");
        crate::task_cli::copy_tree(
            &crate::task_report::workspace().join("fixtures/custom-tasks"),
            &repo.join("custom-tasks"),
        );
        // The exact singleton schema from #664, captured identically by kind and Worker.
        // Two IDs must fail schema validation before provenance admission.
        let kind = repo.join("custom-tasks/triage/kind/structured.json");
        let mut contract: Value = serde_json::from_slice(&std::fs::read(&kind).unwrap()).unwrap();
        contract["outputs"]["triage"]["schema"]["properties"]["source_artifact_ids"]["maxItems"] =
            json!(1);
        write(&kind, &contract);
        write(
            &repo.join("custom-tasks/triage/worker/outputs/triage.schema.json"),
            &contract["outputs"]["triage"]["schema"],
        );
        let mut catalog: Value =
            toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
                .unwrap();
        catalog.as_object_mut().unwrap().remove("code_policy");
        catalog["packages"] = json!({});
        catalog["kinds"] = json!({});
        catalog["providers"] = json!({});
        for kind in ["triage", "capitalize"] {
            for part in ["kind", "worker", "pipeline"] {
                let name = format!("custom/{kind}-{part}");
                let path = format!("custom-tasks/{kind}/{part}");
                catalog["packages"][&name] = json!({"version":"1.0.0","path":path,"digest":review_config::lock::package_digest(&name,&repo.join(path)).unwrap()});
            }
            catalog["kinds"][kind] = json!(format!("custom/{kind}-kind"));
            catalog["providers"][format!("custom/{kind}-worker")] = json!("codex-personal");
        }
        std::fs::write(
            repo.join(".af/task-catalog.toml"),
            toml::to_string(&catalog).unwrap(),
        )
        .unwrap();
        std::fs::write(
            repo.join("AGENTS.md"),
            "Ambient repository rules must not become a structured Worker's source context.",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "custom contracts"]);
        let auth = root.path().join("auth");
        let bin = root.path().join("bin");
        crate::fake_models::install("codex", &bin, &auth);
        let program = bin.join("codex");
        let original = std::fs::read_to_string(&program).unwrap();
        let offset = original.rfind("sys.exit(64)").unwrap();
        let execution = r#"
request=sys.stdin.read()
if request==PROBE_INPUT:
 message='OK'; kind='admission'
else:
 kind='worker'; r=json.loads(request)
 with open(home+'/requests','a') as f: f.write(json.dumps(r)+'\n')
 mode=open(home+'/mode').read() if os.path.exists(home+'/mode') else 'good'
 retry=bool(r['feedback'])
 if mode.startswith('retry') and retry: mode='good'
 if mode=='retry': mode='wrong-source'
 if mode=='retry-malformed': mode='malformed'
 if mode=='retry-schema': mode='two-sources'
 if 'issue' in r['inputs']:
  v=r['inputs']['issue'][0]; p=v['payload']
  value={'schema':'custom.issue-triage/2','source_artifact_ids':[v['artifact_id']],'class':None,'size':None,'pipeline':None,'confidence':0.2,'rationale':'Captured evidence is incomplete.','needs_info':['Supply source evidence.'],'disposition':'needs_info','adr':None,'source_identity':p['captured_identity']}
  port='triage'
 else:
  v=r['inputs']['note'][0]; value={'source_artifact_ids':[v['artifact_id']],'text':v['payload']['text'].upper()}; port='capitalized'
 if mode=='two-sources':value['source_artifact_ids'].append('sha256:'+'f'*64)
 if mode=='wrong-source':value['source_artifact_ids']=['sha256:'+'f'*64]
 if mode=='wrong-type':value['confidence']='not a number';value['text']=9
 outputs={port:[value]}
 if mode=='unknown':outputs['forged_receipt']=[{'acceptance':'satisfied'}]
 if mode=='missing':outputs={}
 message=json.dumps({'schema':'af.worker-reply/1','outputs':outputs})
 if mode=='malformed':message='plain text is not typed output'
with open(home+'/calls','a') as f:f.write(kind+'\n')
if kind=='worker' and mode=='transient':
 print(json.dumps({'type':'turn.failed','error':{'message':'Selected model is at capacity. Please try again in 1ms.'}}))
 sys.exit(1)
if '-o' in sys.argv:
 with open(sys.argv[sys.argv.index('-o')+1],'w') as f:f.write(message)
print(json.dumps({'type':'thread.started','thread_id':'custom-fixture'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':3,'cached_input_tokens':0,'output_tokens':2,'reasoning_output_tokens':0,'cache_write_input_tokens':0}}))
"#;
        std::fs::write(
            &program,
            crate::provider_cli::with_probe(&format!("{}{}", &original[..offset], execution)),
        )
        .unwrap();
        let registry = root.path().join("providers.toml");
        std::fs::write(registry,toml::to_string(&json!({"version":3,"providers":[{"id":"codex-personal","kind":"codex","auth_dir":auth}]})).unwrap()).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            root,
            repo,
            state,
            auth,
            path,
        }
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        crate::common::af()
            .current_dir(&self.repo)
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.root.path().join("providers.toml"))
            .args(args)
            .args(["--state", self.state.to_str().unwrap(), "--json"])
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.cli(args);
        assert!(out.status.success(), "{}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn plan(&self, kind: &str) -> Value {
        let port = if kind == "triage" { "issue" } else { "note" };
        self.ok(&[
            "task",
            "plan",
            "--file",
            &format!("custom-tasks/{kind}/task.json"),
            "--input-json",
            &format!("{port}={port}.json"),
        ])
    }
    fn calls(&self) -> String {
        std::fs::read_to_string(self.auth.join("calls")).unwrap_or_default()
    }
}

#[test]
fn two_custom_kinds_use_the_real_model_adapter_and_structural_acceptance() {
    let f = Fixture::new();
    for kind in ["triage", "capitalize"] {
        let planned = f.plan(kind);
        assert_eq!(planned["attempts"], 0);
        assert_eq!(planned["plan"]["authority"]["allowed_effects"], json!([]));
        let cas = review_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let authority = cas
            .get_json(planned["plan"]["authority"]["policy_id"].as_str().unwrap())
            .unwrap();
        for name in [
            "code_policy_id",
            "document_policy_id",
            "report_policy_id",
            "review_policy_id",
        ] {
            assert!(authority.get(name).is_none(), "{authority}");
        }

        assert_eq!(planned["graph"]["schema"], "af.compiled-task/3");
        crate::schemas::valid(
            &crate::schemas::validator("compiled-task-v3.json"),
            &planned["graph"],
        );
        let mut old_graph: review_graph::task::CompiledTask =
            serde_json::from_value(planned["graph"].clone()).unwrap();
        old_graph.schema = "af.compiled-task/2".into();
        assert!(old_graph.validate_schema().is_err());
        let pipeline_path = f
            .repo
            .join(format!("custom-tasks/{kind}/pipeline/pipeline.toml"));
        let pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
            review_config::task::parse_task_pipeline(
                &std::fs::read_to_string(pipeline_path).unwrap(),
            )
            .unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("pipeline-definition-v4.json"),
            &serde_json::to_value(&pipeline).unwrap(),
        );
        let mut old_pipeline = pipeline;
        old_pipeline.schema = review_core::task::pipeline::PipelineSchema::V1;
        assert!(old_pipeline.validate().is_err());
        let kind_path = f.repo.join(format!("custom-tasks/{kind}/kind/kind.toml"));
        let mut kind_manifest: review_config::task::kind::TaskKindManifest =
            toml::from_str(&std::fs::read_to_string(kind_path).unwrap()).unwrap();
        kind_manifest.validate().unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("task-kind-v2.json"),
            &serde_json::to_value(&kind_manifest).unwrap(),
        );
        let mut report_alias = kind_manifest.clone();
        report_alias.kind = "report".into();
        assert!(report_alias.validate().is_err());
        assert!(
            !crate::schemas::validator("task-kind-v2.json")
                .is_valid(&serde_json::to_value(report_alias).unwrap())
        );
        kind_manifest.schema = "af.task-kind/1".into();
        assert!(kind_manifest.validate().is_err());
        // Mutable source files cannot alter what the recorded plan delivers to its Worker.
        let input_name = if kind == "triage" { "issue" } else { "note" };
        std::fs::write(
            f.repo
                .join(format!("custom-tasks/{kind}/{input_name}.json")),
            "changed after capture",
        )
        .unwrap();
        let binding = &planned["plan"]["bindings"];
        assert!(binding.to_string().contains("gpt-6.1-sol"), "{binding}");
        assert!(binding.to_string().contains("medium"), "{binding}");
        assert!(binding.to_string().contains("codex-personal"), "{binding}");
        assert_eq!(binding.as_object().unwrap().len(), 1);
        let done = f.ok(&[
            "task",
            "run",
            &format!("{kind}-one"),
            "--confirm-plan",
            planned["plan_id"].as_str().unwrap(),
        ]);
        assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
        assert_eq!(
            done["result"]["domain_conclusion"],
            "structurally_validated"
        );
        let port = if kind == "triage" {
            "triage"
        } else {
            "capitalized"
        };
        let output = f.ok(&[
            "task",
            "output",
            &format!("{kind}-one"),
            "--port",
            port,
            "--format",
            "json",
            "--output",
            f.root.path().join(format!("{kind}.json")).to_str().unwrap(),
        ]);
        assert!(
            output["artifact_id"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        let calls = f.calls();
        let replay = f.ok(&["task", "run", &format!("{kind}-one")]);
        assert_eq!(replay["result"], done["result"]);
        assert_eq!(f.calls(), calls);
        let refused = f.cli(&[
            "task",
            "deliver",
            &format!("{kind}-one"),
            "--branch",
            "forbidden",
            "--worktree",
            "../forbidden",
            "--confirm",
            &format!("{kind}-one"),
        ]);
        assert!(!refused.status.success());
        assert!(text(&refused).contains("structural data validation only"));
        let validation = &done["result"]["outputs"]["validation"]["artifact_ids"][0];
        let cas = review_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let receipt = cas.get_artifact(validation.as_str().unwrap()).unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("structured-validation-v1.json"),
            &receipt.payload,
        );
    }
    assert_eq!(f.calls().lines().filter(|s| *s == "worker").count(), 2);
    for line in std::fs::read_to_string(f.auth.join("requests"))
        .unwrap()
        .lines()
    {
        let request: Value = serde_json::from_str(line).unwrap();
        let requirements = request["inputs"]["requirements"][0]["payload"]["text"]
            .as_str()
            .unwrap();
        assert!(!requirements.contains("AGENTS.md"), "{request}");
        assert!(
            !requirements.contains("Ambient repository rules"),
            "{request}"
        );
        assert_eq!(request["inputs"].as_object().unwrap().len(), 2);
    }
}

#[test]
fn malformed_unknown_missing_wrong_type_and_wrong_source_outputs_never_accept() {
    let f = Fixture::new();
    let path = f.repo.join("custom-tasks/triage/task.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for mode in [
        "malformed",
        "unknown",
        "missing",
        "wrong-type",
        "wrong-source",
        "two-sources",
        "retry",
        "retry-malformed",
        "retry-schema",
    ] {
        std::fs::write(f.auth.join("mode"), mode).unwrap();
        task["task_id"] = json!(mode);
        write(&path, &task);
        let before = f.calls();
        let plan = f.plan("triage");
        let out = f.cli(&[
            "task",
            "run",
            mode,
            "--confirm-plan",
            plan["plan_id"].as_str().unwrap(),
        ]);
        let done: Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", text(&out)));
        assert_eq!(
            f.calls().strip_prefix(&before).unwrap(),
            "admission\nworker\nworker\n",
            "{mode}"
        );
        assert_eq!(done["attempts"], 3, "{done}");
        let rows = accounting(&f, mode);
        assert_eq!(rows.len(), 3);
        for row in rows {
            assert_eq!(
                row.charged_tokens, 5,
                "one call charged per Attempt: {mode}"
            );
            assert!(row.unknown_usage.is_none());
            assert!(row.settled_unix_ms.is_some());
            assert!(!row.raw_artifact_ids.is_empty());
        }
        if mode.starts_with("retry") {
            assert!(out.status.success(), "{}", text(&out));
            assert_eq!(done["result"]["acceptance"], "satisfied");
        } else {
            assert!(!out.status.success(), "{done}");
            assert_ne!(done["result"]["acceptance"], "satisfied");
            assert!(done["result"]["outputs"].get("validation").is_none());
        }
    }
    let requests = std::fs::read_to_string(f.auth.join("requests")).unwrap();
    let requests: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 18);
    for pair in requests.chunks_exact(2) {
        assert!(pair[0]["feedback"].as_array().unwrap().is_empty());
        let feedback = pair[1]["feedback"].as_array().unwrap();
        assert!(!feedback.is_empty());
        assert!(
            feedback
                .iter()
                .any(|value| value["artifact_type"] == "af/TaskRetryFeedback@1")
        );
        assert!(
            feedback
                .iter()
                .all(|value| value["artifact_type"] != "af/WorkerReplyReask@1")
        );
    }
}

#[test]
fn external_inputs_fail_before_dispatch_and_exact_bytes_and_schemas_change_identity() {
    let f = Fixture::new();
    let input = f.repo.join("custom-tasks/triage/issue.json");
    let original = std::fs::read(&input).unwrap();
    let first = f.plan("triage");
    assert!(f.calls().is_empty());
    let mut issue: Value = serde_json::from_slice(&original).unwrap();
    issue["title"] = json!("Changed bytes");
    write(&input, &issue);
    let revised = f.ok(&[
        "task",
        "plan",
        "--replace",
        "--file",
        "custom-tasks/triage/task.json",
        "--input-json",
        "issue=issue.json",
    ]);
    assert_ne!(first["plan_id"], revised["plan_id"]);
    assert_ne!(
        first["graph"]["inputs"]["issue"]["artifact_ids"],
        revised["graph"]["inputs"]["issue"]["artifact_ids"]
    );
    std::fs::write(&input, &original).unwrap();
    for binding in [
        None,
        Some("unknown=issue.json"),
        Some("issue=absent.json"),
        Some("issue=../triage/issue.json"),
        Some("issue=/etc/passwd"),
    ] {
        let mut args = vec![
            "task",
            "plan",
            "--replace",
            "--file",
            "custom-tasks/triage/task.json",
        ];
        if let Some(b) = binding {
            args.extend(["--input-json", b]);
        }
        let out = f.cli(&args);
        assert!(!out.status.success(), "{binding:?}: {}", text(&out));
        assert!(f.calls().is_empty());
    }
    for bad in [b"not json".to_vec(), vec![b' '; 65537], b"{}".to_vec()] {
        std::fs::write(&input, bad).unwrap();
        let out = f.cli(&[
            "task",
            "plan",
            "--replace",
            "--file",
            "custom-tasks/triage/task.json",
            "--input-json",
            "issue=issue.json",
        ]);
        assert!(!out.status.success());
        assert!(f.calls().is_empty());
    }
    std::fs::remove_file(&input).unwrap();
    std::os::unix::fs::symlink(f.root.path().join("outside.json"), &input).unwrap();
    write(&f.root.path().join("outside.json"), &issue);
    let out = f.cli(&[
        "task",
        "plan",
        "--replace",
        "--file",
        "custom-tasks/triage/task.json",
        "--input-json",
        "issue=issue.json",
    ]);
    assert!(!out.status.success());
    assert!(f.calls().is_empty());
    std::os::unix::fs::symlink(f.root.path(), f.repo.join("custom-tasks/triage/redirect")).unwrap();
    let out = f.cli(&[
        "task",
        "plan",
        "--replace",
        "--file",
        "custom-tasks/triage/task.json",
        "--input-json",
        "issue=redirect/outside.json",
    ]);
    assert!(!out.status.success());
    assert!(f.calls().is_empty());
}

#[test]
fn structured_contract_version_is_guarded_at_digest_and_planning_before_model_calls() {
    let f = Fixture::new();
    let path = f.repo.join("custom-tasks/triage/kind/structured.json");
    let mut contract: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let digest = || {
        crate::common::af()
            .current_dir(&f.repo)
            .args(["catalog", "digest", "custom-tasks/triage/kind"])
            .output()
            .unwrap()
    };
    assert!(digest().status.success());
    let current = f.plan("triage");
    assert_eq!(current["attempts"], 0);
    // The exact-version check also protects an older client from a newer contract:
    // a future version cannot acquire authority by carrying a correct package digest.
    for version in ["af.structured-contract/1", "af.structured-contract/3"] {
        contract["schema"] = json!(version);
        write(&path, &contract);
        let out = digest();
        assert!(!out.status.success(), "{}", text(&out));
        assert!(
            text(&out).contains("Unsupported structured contract"),
            "{}",
            text(&out)
        );
        // Bypass the friendly digest command and pin the exact unsupported bytes, so
        // planning must exercise package validation rather than a stale-pin refusal.
        crate::task_report::repin_and_commit(&f.repo);
        let out = f.cli(&[
            "task",
            "plan",
            "--replace",
            "--file",
            "custom-tasks/triage/task.json",
            "--input-json",
            "issue=issue.json",
        ]);
        assert!(!out.status.success(), "{}", text(&out));
        assert!(
            text(&out).contains("Unsupported structured contract"),
            "{}",
            text(&out)
        );
        assert!(f.calls().is_empty());
        assert!(!f.auth.join("requests").exists());
    }
}

#[test]
fn schema_mutation_changes_plan_and_remote_schemas_and_reserved_types_refuse() {
    let f = Fixture::new();
    let first = f.plan("triage");
    let path = f.repo.join("custom-tasks/triage/kind/structured.json");
    let mut contract: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("structured-contract-v2.json"),
        &contract,
    );
    contract["inputs"]["issue"]["schema"]["properties"]["title"]["maxLength"] = json!(511);
    write(&path, &contract);
    crate::task_report::repin_and_commit(&f.repo);
    let changed = f.ok(&[
        "task",
        "plan",
        "--replace",
        "--file",
        "custom-tasks/triage/task.json",
        "--input-json",
        "issue=issue.json",
    ]);
    assert_ne!(first["plan_id"], changed["plan_id"]);
    assert_ne!(
        first["plan"]["dependencies"]["custom/triage-kind"]["artifact_id"],
        changed["plan"]["dependencies"]["custom/triage-kind"]["artifact_id"]
    );
    assert_ne!(
        first["graph"]["inputs"]["issue"]["artifact_ids"],
        changed["graph"]["inputs"]["issue"]["artifact_ids"]
    );
    assert!(f.calls().is_empty());
    let prompt = f.repo.join("custom-tasks/triage/worker/instructions.md");
    let prompt_text = std::fs::read_to_string(&prompt).unwrap();
    std::fs::write(
        &prompt,
        format!("{prompt_text}\nRetain the exact captured source identity.\n"),
    )
    .unwrap();
    crate::task_report::repin_and_commit(&f.repo);
    let worker_changed = f.ok(&[
        "task",
        "plan",
        "--replace",
        "--file",
        "custom-tasks/triage/task.json",
        "--input-json",
        "issue=issue.json",
    ]);
    assert_ne!(changed["plan_id"], worker_changed["plan_id"]);
    assert_ne!(
        changed["plan"]["dependencies"]["custom/triage-worker"]["artifact_id"],
        worker_changed["plan"]["dependencies"]["custom/triage-worker"]["artifact_id"]
    );
    assert!(f.calls().is_empty());
    for (field, bad) in [
        ("schema", json!({"$ref":"https://example.invalid/schema"})),
        ("artifact_type", json!("af/StructuredValidation@1")),
    ] {
        let original = contract.clone();
        contract["inputs"]["issue"][field] = bad;
        write(&path, &contract);
        // Deliberately compute the pin without `catalog digest`: admission must refuse even
        // authority that pins a malicious schema exactly.
        crate::task_report::repin_and_commit(&f.repo);
        let out = f.cli(&[
            "task",
            "plan",
            "--replace",
            "--file",
            "custom-tasks/triage/task.json",
            "--input-json",
            "issue=issue.json",
        ]);
        assert!(!out.status.success(), "{}", text(&out));
        assert!(f.calls().is_empty());
        contract = original;
    }
}

#[test]
fn native_scenario_projects_the_exact_custom_output_without_an_extra_worker() {
    let f = Fixture::new();
    let planned = f.plan("triage");
    let done = f.ok(&[
        "task",
        "run",
        "triage-one",
        "--confirm-plan",
        planned["plan_id"].as_str().unwrap(),
    ]);
    let calls = f.calls();
    let mut bundle: Value = serde_json::from_slice(
        &std::fs::read(
            crate::task_report::workspace().join("fixtures/custom-tasks/triage/scenario.json"),
        )
        .unwrap(),
    )
    .unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("scenario-bundle-v5.json"),
        &bundle,
    );
    bundle["definitions"]["triage"]["steps"][0]["operation"]["target"] = json!({"repo":f.repo,"state":f.state,"task_id":"triage-one","plan_id":planned["plan_id"],"revision_id":planned["revision_id"],"max_tokens":150000});
    bundle["definitions"]["triage"]["authority"]["task_plans"] = json!([planned["plan_id"]]);
    let bundle_path = f.root.path().join("scenario.json");
    write(&bundle_path, &bundle);
    let policy = f.root.path().join("policy.json");
    write(
        &policy,
        &json!({"schema":"af/scenario-policy@5","adapters":{},"task_plans":[planned["plan_id"]],"task_templates":[],"decision_keys":{},"file_roots":[]}),
    );
    crate::schemas::valid(
        &crate::schemas::validator("scenario-policy-v5.json"),
        &serde_json::from_slice(&std::fs::read(&policy).unwrap()).unwrap(),
    );
    std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
    let inputs = f.root.path().join("inputs.json");
    write(&inputs, &json!({}));
    let checked = f.ok(&[
        "scenario",
        "validate",
        "--file",
        bundle_path.to_str().unwrap(),
        "--authority",
        policy.to_str().unwrap(),
    ]);
    let started = f.ok(&[
        "scenario",
        "start",
        "custom-triage",
        "--file",
        bundle_path.to_str().unwrap(),
        "--inputs",
        inputs.to_str().unwrap(),
        "--authority",
        policy.to_str().unwrap(),
        "--confirm-pin",
        checked["revision"].as_str().unwrap(),
        "--execute",
    ]);
    assert_eq!(started["status"], "succeeded", "{started}");
    assert_eq!(
        started["frames"]["root"]["outputs"]["triage"]["value"],
        done["result"]["outputs"]["triage"]["artifact_ids"][0]
    );
    f.ok(&["scenario", "resume", "custom-triage"]);
    assert_eq!(f.calls(), calls);
}

#[test]
fn compatible_recorded_custom_outputs_bind_through_the_existing_root_input_path() {
    let f = Fixture::new();
    let base = f.repo.join("custom-tasks/capitalize");
    let path = base.join("kind/structured.json");
    let mut contract: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    contract["inputs"]["note"]["artifact_type"] = json!("custom/Capitalized@1");
    contract["inputs"]["note"]["schema"]["properties"]["source_artifact_ids"] =
        contract["outputs"]["capitalized"]["schema"]["properties"]["source_artifact_ids"].clone();
    write(&path, &contract);
    let path = base.join("worker/worker.toml");
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("custom/Note@1", "custom/Capitalized@1"),
    )
    .unwrap();
    let path = base.join("pipeline/pipeline.toml");
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("custom/Note@1", "custom/Capitalized@1"),
    )
    .unwrap();
    let path = base.join("worker/input.schema.json");
    let mut schema: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    schema["properties"]["note"]["items"]["properties"]["artifact_type"]["const"] =
        json!("custom/Capitalized@1");
    schema["properties"]["note"]["items"]["properties"]["payload"] =
        contract["inputs"]["note"]["schema"].clone();
    write(&path, &schema);
    crate::task_report::repin_and_commit(&f.repo);
    let first = f.plan("capitalize");
    let done = f.ok(&[
        "task",
        "run",
        "capitalize-one",
        "--confirm-plan",
        first["plan_id"].as_str().unwrap(),
    ]);
    let path = base.join("task.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    task["task_id"] = json!("capitalize-bound");
    task["inputs"] = json!({"note":{"task":"capitalize-one","port":"capitalized"}});
    write(&path, &task);
    let next = f.ok(&[
        "task",
        "plan",
        "--file",
        "custom-tasks/capitalize/task.json",
    ]);
    assert_eq!(
        next["graph"]["inputs"]["note"]["artifact_ids"],
        done["result"]["outputs"]["capitalized"]["artifact_ids"]
    );
    let bound = f.ok(&[
        "task",
        "run",
        "capitalize-bound",
        "--confirm-plan",
        next["plan_id"].as_str().unwrap(),
    ]);
    assert_eq!(
        bound["result"]["domain_conclusion"],
        "structurally_validated"
    );
}

#[test]
fn staged_business_schema_parity_preserves_held_missing_information_and_rubric_routes() {
    let root = crate::task_report::workspace();
    let contract: Value = serde_json::from_slice(
        &std::fs::read(root.join("fixtures/custom-tasks/triage/kind/structured.json")).unwrap(),
    )
    .unwrap();
    for (schema, direction, port) in [
        ("custom-issue-input-v1.json", "inputs", "issue"),
        ("custom-issue-triage-v2.json", "outputs", "triage"),
    ] {
        let mut published: Value =
            serde_json::from_slice(&std::fs::read(root.join("schemas").join(schema)).unwrap())
                .unwrap();
        published.as_object_mut().unwrap().remove("$id");
        published.as_object_mut().unwrap().remove("$schema");
        assert_eq!(published, contract[direction][port]["schema"]);
    }
    let validator = crate::schemas::validator("custom-issue-triage-v2.json");
    let identity = json!({"revision":"captured","captured_at":"unknown","body_sha256":format!("sha256:{}","a".repeat(64))});
    let mut value = json!({"schema":"custom.issue-triage/2","source_artifact_ids":[format!("sha256:{}","b".repeat(64))],"class":null,"size":null,"pipeline":null,"confidence":0.2,"rationale":"Missing evidence.","needs_info":["Supply reproduction."],"disposition":"needs_info","adr":null,"source_identity":identity});
    crate::schemas::valid(&validator, &value);
    value["class"] = json!("bug");
    assert!(!validator.is_valid(&value));
    value["class"] = json!("security");
    value["disposition"] = json!("held");
    crate::schemas::valid(&validator, &value);
    value["disposition"] = json!("proposed");
    assert!(!validator.is_valid(&value));
    value["class"] = json!("bug");
    value["confidence"] = json!(0.9);
    value["needs_info"] = json!([]);
    for (size, pipeline) in [
        ("S", "light"),
        ("M", "default"),
        ("L", "design"),
        ("XL", "design"),
    ] {
        value["size"] = json!(size);
        value["pipeline"] = json!(pipeline);
        crate::schemas::valid(&validator, &value);
        value["pipeline"] = json!("hand");
        assert!(!validator.is_valid(&value));
    }
}

/// Admission and a single Worker exhaust the exact two-Attempt plan: no hidden re-ask or
/// transient recovery is allowed, including schema maxItems failures and wrong provenance.
#[test]
fn structured_single_attempt_makes_one_worker_call_and_fails_closed() {
    let f = Fixture::new();
    let pipeline = f.repo.join("custom-tasks/triage/pipeline/pipeline.toml");
    std::fs::write(
        &pipeline,
        std::fs::read_to_string(&pipeline)
            .unwrap()
            .replace("max_attempts = 8", "max_attempts = 2")
            .replace(
                "max_attempts = 2\nallow_local",
                "max_attempts = 1\nallow_local",
            ),
    )
    .unwrap();
    crate::task_report::repin_and_commit(&f.repo);
    let path = f.repo.join("custom-tasks/triage/task.json");
    let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    task["limits"]["max_attempts"] = json!(2);
    for mode in [
        "good",
        "malformed",
        "wrong-type",
        "two-sources",
        "wrong-source",
        "transient",
    ] {
        std::fs::write(f.auth.join("mode"), mode).unwrap();
        task["task_id"] = json!(mode);
        write(&path, &task);
        let before = f.calls();
        let plan = f.plan("triage");
        let out = f.cli(&[
            "task",
            "run",
            mode,
            "--confirm-plan",
            plan["plan_id"].as_str().unwrap(),
        ]);
        let done: Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{}", text(&out)));
        assert_eq!(
            out.status.success(),
            mode == "good",
            "{mode}: {}",
            text(&out)
        );
        assert_eq!(
            f.calls().strip_prefix(&before).unwrap(),
            "admission\nworker\n",
            "{mode}"
        );
        assert_eq!(done["attempts"], 2, "{done}");
        let rows = accounting(&f, mode);
        assert_eq!(rows.len(), 2);
        let cas = review_store::Cas::open_existing(f.state.join("cas")).unwrap();
        for row in rows {
            if mode != "good" && row.reservation.node.ends_with(".work") {
                let Some(review_core::task::execution::TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    feedback_id: Some(feedback_id),
                }) = &row.result
                else {
                    panic!("Worker failure must retain typed feedback: {mode}")
                };
                let feedback = cas.get_artifact(feedback_id).unwrap();
                let expected = match mode {
                    "wrong-source" => "output_admission_rejected",
                    "transient" => "provider_failure",
                    _ => "invalid_output_contract",
                };
                assert_eq!(feedback.payload["code"], expected, "{mode}");
                if mode == "two-sources" {
                    let diagnostic = cas.get_json(diagnostic_id).unwrap();
                    let error = diagnostic["error"].as_str().unwrap();
                    assert!(error.contains("Worker output triage:"), "{error}");
                    assert!(error.contains("not re-asked:"), "{error}");
                }
            }
            let transient_worker = mode == "transient" && row.reservation.node.ends_with(".work");
            assert_eq!(
                row.charged_tokens,
                if transient_worker { 0 } else { 5 },
                "{mode}"
            );
            assert_eq!(row.unknown_usage.is_some(), transient_worker, "{mode}");
            assert!(row.settled_unix_ms.is_some());
            assert!(!row.raw_artifact_ids.is_empty());
        }
        assert_eq!(
            done["result"]["acceptance"] == "satisfied",
            mode == "good",
            "{done}"
        );
        if mode != "good" {
            assert!(done["result"]["outputs"].get("validation").is_none());
        }
        let calls = f.calls();
        let _ = f.cli(&["task", "run", mode]);
        assert_eq!(f.calls(), calls, "finished Task replay dispatches nothing");
    }
}

fn accounting(
    f: &Fixture,
    task: &str,
) -> Vec<review_store::store::task::execution::TaskAttemptAccounting> {
    let cas = review_store::Cas::open_existing(f.state.join("cas")).unwrap();
    review_store::EventStore::open_read_only(f.state.join("events.sqlite"))
        .unwrap()
        .task_projection(&cas, task)
        .unwrap()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting()
}
