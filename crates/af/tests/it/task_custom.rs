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
        // The harness home is writable even when the sandbox denies the ambient temp directory.
        let home = crate::common::private_home_env()
            .into_iter()
            .find(|(name, _)| *name == "HOME")
            .unwrap()
            .1;
        let root = tempfile::tempdir_in(home).unwrap();
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
        let pipeline = repo.join("custom-tasks/triage/pipeline/pipeline.toml");
        let text = std::fs::read_to_string(&pipeline).unwrap();
        std::fs::write(
            &pipeline,
            text.replace(
                "role = \"triage\"",
                "role = \"triage\"\nrecovery_attempts = 2",
            ),
        )
        .unwrap();
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
                let manifest: Value = toml::from_str(
                    &std::fs::read_to_string(repo.join(&path).join(format!("{part}.toml")))
                        .unwrap(),
                )
                .unwrap();
                let version = manifest["version"].as_str().unwrap();
                catalog["packages"][&name] = json!({"version":version,"path":path,"digest":af_pipeline_config::lock::package_digest(&name,&repo.join(path)).unwrap()});
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
        std::fs::write(registry,toml::to_string(&json!({"version":4,"providers":[{"id":"codex-personal","kind":"codex","auth_dir":auth}]})).unwrap()).unwrap();
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
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
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

        assert_eq!(planned["graph"]["schema"], "af.compiled-task/6");
        crate::schemas::valid(
            &crate::schemas::validator("compiled-task-v6.json"),
            &planned["graph"],
        );
        for retired in [
            "af.compiled-task/1",
            "af.compiled-task/2",
            "af.compiled-task/3",
        ] {
            let mut old_graph: af_graph::task::CompiledTask =
                serde_json::from_value(planned["graph"].clone()).unwrap();
            old_graph.schema = retired.into();
            assert!(old_graph.validate_schema().is_err());
            assert!(
                !crate::schemas::validator("compiled-task-v6.json")
                    .is_valid(&serde_json::to_value(old_graph).unwrap())
            );
        }
        if kind == "triage" {
            assert_eq!(
                planned["graph"]["slots"]["root.slots.triage"]["recovery_attempts"],
                2
            );
        }
        let pipeline_path = f
            .repo
            .join(format!("custom-tasks/{kind}/pipeline/pipeline.toml"));
        let pipeline: af_contracts::task::pipeline::PipelineDefinition =
            af_pipeline_config::task::parse_task_pipeline(
                &std::fs::read_to_string(pipeline_path).unwrap(),
            )
            .unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("pipeline-definition-v6.json"),
            &serde_json::to_value(&pipeline).unwrap(),
        );
        let mut old_pipeline = serde_json::to_value(&pipeline).unwrap();
        old_pipeline["schema"] = json!("af.pipeline/4");
        assert!(
            serde_json::from_value::<af_contracts::task::pipeline::PipelineDefinition>(
                old_pipeline
            )
            .is_err()
        );
        let kind_path = f.repo.join(format!("custom-tasks/{kind}/kind/kind.toml"));
        let mut kind_manifest: af_pipeline_config::task::kind::TaskKindManifest =
            toml::from_str(&std::fs::read_to_string(kind_path).unwrap()).unwrap();
        kind_manifest.validate().unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("task-kind-v3.json"),
            &serde_json::to_value(&kind_manifest).unwrap(),
        );
        let mut report_alias = kind_manifest.clone();
        report_alias.kind = "report".into();
        assert!(report_alias.validate().is_err());
        assert!(
            !crate::schemas::validator("task-kind-v3.json")
                .is_valid(&serde_json::to_value(report_alias).unwrap())
        );
        kind_manifest.schema = "af.task-kind/2".into();
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
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
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
        &crate::schemas::validator("scenario-bundle-v6.json"),
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
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
        for row in rows {
            if mode != "good" && row.reservation.node.ends_with(".work") {
                let Some(af_contracts::task::execution::TaskAttemptResultV1::Failed {
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
) -> Vec<af_store::store::task::execution::TaskAttemptAccounting> {
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    af_store::EventStore::open_read_only(f.state.join("events.sqlite"))
        .unwrap()
        .task_projection(&cas, task)
        .unwrap()
        .unwrap()
        .execution
        .unwrap()
        .attempt_accounting()
}

impl Fixture {
    fn ci_health_release(main_failure: bool) -> Self {
        let home = crate::common::private_home_env()
            .into_iter()
            .find(|(name, _)| *name == "HOME")
            .unwrap()
            .1;
        let scratch = tempfile::tempdir_in(home).unwrap();
        let source = crate::task_report::workspace();
        let output = std::process::Command::new("python3")
            .args(["-I", "-c", r#"
import importlib.util,json,pathlib,sys
sys.dont_write_bytecode=True
root=pathlib.Path(sys.argv[1]); scratch=pathlib.Path(sys.argv[2])
spec=importlib.util.spec_from_file_location('ci',root/'scripts/ci-health.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)
fixture=root/'fixtures/scenarios/maintenance-ci-health'
github=json.loads((fixture/'release-github-service.json').read_text())
if sys.argv[3]=='false':
    github['routes']['repos/octo/project/actions/runs/101/attempts/1/jobs?per_page=100']['jobs'][0]['conclusion']='success'
raw=json.loads((fixture/'failing-service.json').read_text())
service=c.collect(c.OfflineGithub(github['repository'],github['routes'],github['max_requests']),
    captured_at=raw['captured_at'],main_ref=github['main_ref'],since=github['since'],
    nightly_receipt=raw['nightly_receipt'],release_pr=7)
journal=c.Journal(scratch/'observer.sqlite')
try: snapshot=c.observe(service,journal)
finally: journal.close()
(scratch/'snapshot.json').write_bytes(c.canonical(snapshot))
print(json.dumps({'observation_service':service,'github_service':github}))
"#])
            .arg(&source)
            .arg(scratch.path())
            .arg(main_failure.to_string())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        let f = Self::ci_health_from_snapshot(&scratch.path().join("snapshot.json"));
        std::fs::write(f.root.path().join("ci-service.json"), output.stdout).unwrap();
        f
    }
    fn ci_health() -> Self {
        Self::ci_health_from_snapshot(
            &crate::task_report::workspace()
                .join("fixtures/scenarios/maintenance-ci-health/snapshot.json"),
        )
    }
    fn ci_health_from_snapshot(snapshot_path: &Path) -> Self {
        let f = Self::new();
        let source =
            crate::task_report::workspace().join("fixtures/scenarios/maintenance-ci-health");
        crate::task_cli::copy_tree(&source.join("task"), &f.repo.join("ci-health"));
        let snapshot_bytes = std::fs::read(snapshot_path).unwrap();
        // A Gate seals this source read-only. Copy its exact bytes into a new disposable
        // fixture file without inheriting the source's mode: these tests mutate the snapshot.
        std::fs::write(f.repo.join("ci-health/snapshot.json"), &snapshot_bytes).unwrap();
        let snapshot: Value = serde_json::from_slice(&snapshot_bytes).unwrap();
        let task_path = f.repo.join("ci-health/task.json");
        let mut task: Value = serde_json::from_slice(&std::fs::read(&task_path).unwrap()).unwrap();
        task["requirements"] = json!({"schema":"af.ci-diagnosis-selection/1","occurrence_id":snapshot["failures"][0]["occurrence_id"],
            "signature_id":snapshot["failures"][0]["signature_id"],"source_sha":snapshot["failures"][0]["head_sha"]});
        write(&task_path, &task);
        let path = f.repo.join(".af/task-catalog.toml");
        let mut catalog: Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for part in ["kind", "worker", "pipeline"] {
            let name = format!("ci/diagnosis-{part}");
            let package = format!("ci-health/{part}");
            let manifest: Value = toml::from_str(
                &std::fs::read_to_string(f.repo.join(&package).join(format!("{part}.toml")))
                    .unwrap(),
            )
            .unwrap();
            catalog["packages"][&name] = json!({"version":manifest["version"],"path":package,
                "digest":af_pipeline_config::lock::package_digest(&name,&f.repo.join(package)).unwrap()});
        }
        catalog["kinds"]["ci-diagnosis"] = json!("ci/diagnosis-kind");
        catalog["providers"]["ci/diagnosis-worker"] = json!("codex-personal");
        std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
        git(&f.repo, &["add", "-A"]);
        git(
            &f.repo,
            &["commit", "-qm", "hermetic CI diagnosis contract"],
        );
        let program = f.root.path().join("bin/codex");
        let original = std::fs::read_to_string(&program).unwrap();
        let branch = r#"if 'snapshot' in r['inputs']:
  v=r['inputs']['snapshot'][0]; s=v['payload']; sid=v['artifact_id']
  selected=r['inputs']['requirements'][0]['payload']['specification']
  failure=next(f for f in s['failures'] if f['occurrence_id']==selected['occurrence_id'])
  assert selected['source_sha']==failure['head_sha']
  category={'ci-transient':'transient','ci-test':'test_bug','ci-infra':'ci_infra','ci-unknown':'unknown'}.get(mode,'product_bug')
  value={'schema':'af.ci-diagnosis/1','snapshot_id':sid,'source_artifact_ids':[sid],
    'occurrence_id':failure['occurrence_id'],'signature_id':failure['signature_id'],'source_sha':selected['source_sha'],
    'category':category,'rationale':'Hermetic exact assertion evidence','evidence':[{'snapshot_id':sid,
    'source_sha':selected['source_sha'],'reference':'snapshot'}]};port='diagnosis'
  if mode=='ci-main-revision':
    value['source_sha']=s['source_sha'];value['evidence'][0]['source_sha']=s['source_sha']
  if mode=='ci-foreign-signature':value['signature_id']='sha256:'+'f'*64
  if mode=='ci-foreign-occurrence':value['occurrence_id']='sha256:'+'f'*64
  if mode=='ci-foreign-evidence':value['evidence'][0]['reference']='sha256:'+'f'*64
  if mode=='ci-malformed-evidence':value['evidence']=[]
 elif 'issue' in r['inputs']:"#;
        assert!(original.contains("if 'issue' in r['inputs']:"));
        std::fs::write(
            program,
            original.replace("if 'issue' in r['inputs']:", branch),
        )
        .unwrap();
        f
    }
    fn ci_plan(&self) -> Value {
        self.ok(&[
            "task",
            "plan",
            "--file",
            "ci-health/task.json",
            "--input-json",
            "snapshot=snapshot.json",
        ])
    }
    fn ci_scenario(&self, planned: &Value, snapshot: &Value, options: &Value) -> (Value, PathBuf) {
        let revision = af_store::Cas::open(self.state.join("cas"))
            .unwrap()
            .get_artifact(planned["revision_id"].as_str().unwrap())
            .unwrap();
        let snapshot_id = revision.payload["inputs"]["snapshot"]["artifact_ids"][0]
            .as_str()
            .unwrap();
        let captured = af_store::Cas::open(self.state.join("cas"))
            .unwrap()
            .get_artifact(snapshot_id)
            .unwrap();
        assert_eq!(
            captured.payload, *snapshot,
            "Fixture observation must be the exact typed Task input artifact"
        );
        let machine = self.root.path().join("ci-machine");
        std::fs::create_dir(&machine).unwrap();
        std::fs::set_permissions(&machine, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source =
            crate::task_report::workspace().join("fixtures/scenarios/maintenance-ci-health");
        let adapter = machine.join("adapter");
        let production = self.root.path().join("ci-production-adapter");
        let installed = if production.is_file() {
            std::fs::read(production).unwrap()
        } else {
            let output = std::process::Command::new("python3").args(["-I", "-c",
                "import importlib.util,sys,pathlib;s=importlib.util.spec_from_file_location('config',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);sys.stdout.buffer.write(m.adapter_bytes(pathlib.Path(sys.argv[2])))"])
                .arg(source.join("configure.py")).arg(crate::task_report::workspace()).output().unwrap();
            assert!(output.status.success(), "{}", text(&output));
            output.stdout
        };
        std::fs::write(&adapter, installed).unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut observation_service: Value =
            serde_json::from_slice(&std::fs::read(source.join("failing-service.json")).unwrap())
                .unwrap();
        observation_service["omissions"] = snapshot["omissions"].clone();
        let custom_service = self.root.path().join("ci-service.json");
        let mut service = if custom_service.is_file() {
            serde_json::from_slice::<Value>(&std::fs::read(custom_service).unwrap()).unwrap()
        } else {
            json!({"observation_service":observation_service,
                "github_service":serde_json::from_slice::<Value>(&std::fs::read(source.join("github-service.json")).unwrap()).unwrap()})
        };
        service["snapshot"] = snapshot.clone();
        service["snapshot_id"] = json!(snapshot_id);
        service["scenario_run"] = json!("ci-health-fixture");
        service["merged"] = json!(false);
        write(&machine.join("service.json"), &service);
        let settings = self.root.path().join("ci-settings.json");
        write(
            &settings,
            &json!({"snapshot":snapshot,"snapshot_id":snapshot_id,
            "target":{"repo":self.repo,"state":self.state,"task_id":"ci-diagnose-one",
                "plan_id":planned["plan_id"],"revision_id":planned["revision_id"],"max_tokens":24096},"options":options}),
        );
        let output=std::process::Command::new("python3").args(["-I","-c",
            "import importlib.util,json,sys; s=importlib.util.spec_from_file_location('config',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);v=json.load(open(sys.argv[2]));print(json.dumps({'bundle':m.bundle(v['snapshot'],v['snapshot_id'],v['target'],**v['options']),'inputs':m.scope_inputs(v['snapshot'])}))"])
            .arg(source.join("configure.py")).arg(settings).output().unwrap();
        assert!(output.status.success(), "{}", text(&output));
        let rendered: Value = serde_json::from_slice(&output.stdout).unwrap();
        let bundle = rendered["bundle"].clone();
        crate::schemas::valid(
            &crate::schemas::validator("scenario-bundle-v6.json"),
            &bundle,
        );
        let path = self.root.path().join("ci-scenario.json");
        write(&path, &bundle);
        let policy = self.root.path().join("ci-policy.json");
        use sha2::Digest;
        let digest = format!(
            "sha256:{}",
            af_contracts::hex::encode(&sha2::Sha256::digest(std::fs::read(&adapter).unwrap()))
        );
        write(
            &policy,
            &json!({"schema":"af/scenario-policy@5","adapters":{"ci-health":{
            "executable":adapter,"digest":digest,"operations":["observe","notify"],"destinations":["fixture:ci-health"],
            "max_wall_ms":30000,"max_output_bytes":65536}},"task_plans":bundle["definitions"]["maintenance-ci-health"]["authority"]["task_plans"],
            "task_templates":[],"decision_keys":{},"file_roots":[]}),
        );
        std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
        let inputs = self.root.path().join("ci-inputs.json");
        write(&inputs, &rendered["inputs"]);
        let checked = self.ok(&[
            "scenario",
            "validate",
            "--file",
            path.to_str().unwrap(),
            "--authority",
            policy.to_str().unwrap(),
        ]);
        let started = self.ok(&[
            "scenario",
            "start",
            "ci-health-fixture",
            "--file",
            path.to_str().unwrap(),
            "--inputs",
            inputs.to_str().unwrap(),
            "--authority",
            policy.to_str().unwrap(),
            "--confirm-pin",
            checked["revision"].as_str().unwrap(),
            "--execute",
        ]);
        (started, machine)
    }
}

#[test]
fn ci_health_minimum_budget_plans_and_refuses_one_less_token_or_attempt_before_admission() {
    let f = Fixture::ci_health();
    let path = f.repo.join("ci-health/task.json");
    let task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(task["limits"]["tokens"], 24096);
    assert_eq!(task["limits"]["max_attempts"], 2);
    f.ci_plan();
    for (tokens, attempts) in [(24095, 2), (24096, 1)] {
        let mut insufficient = task.clone();
        insufficient["task_id"] = json!(format!("ci-under-{tokens}-{attempts}"));
        insufficient["limits"]["tokens"] = json!(tokens);
        insufficient["limits"]["max_attempts"] = json!(attempts);
        write(&path, &insufficient);
        let output = f.cli(&[
            "task",
            "plan",
            "--file",
            "ci-health/task.json",
            "--input-json",
            "snapshot=snapshot.json",
        ]);
        assert!(!output.status.success(), "{}", text(&output));
        let diagnostic = text(&output);
        assert!(diagnostic.contains("tokens >= 24096"), "{diagnostic}");
        assert!(diagnostic.contains("max_attempts >= 2"), "{diagnostic}");
    }
    assert!(
        f.calls().is_empty(),
        "Planning must not dispatch a Provider"
    );
}

#[test]
fn ci_health_maximum_complete_snapshot_reaches_worker_and_returns_accepted_selected_diagnosis() {
    let f = Fixture::ci_health_from_snapshot(
        &crate::task_report::workspace()
            .join("fixtures/scenarios/maintenance-ci-health/max-snapshot.json"),
    );
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    assert_eq!(snapshot["complete"], true);
    assert_eq!(snapshot["failures"].as_array().unwrap().len(), 100);
    assert_eq!(serde_json::to_vec(&snapshot).unwrap().len(), 65_536);
    let selected = &snapshot["failures"][0];
    let planned = f.ci_plan();
    let done = f.ok(&[
        "task",
        "run",
        "ci-diagnose-one",
        "--confirm-plan",
        planned["plan_id"].as_str().unwrap(),
    ]);
    assert_eq!(done["result"]["execution"], "completed", "{done:#}");
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done:#}");
    assert_eq!(f.calls(), "admission\nworker\n");
    let requests = std::fs::read_to_string(f.auth.join("requests")).unwrap();
    assert_eq!(requests.lines().count(), 1);
    let request: Value = serde_json::from_str(requests.lines().next().unwrap()).unwrap();
    assert_eq!(request["inputs"]["snapshot"][0]["payload"], snapshot);
    assert_eq!(
        request["inputs"]["requirements"][0]["payload"]["specification"]["occurrence_id"],
        selected["occurrence_id"]
    );
    assert_eq!(serde_json::to_vec(&request).unwrap().len(), 67_614);
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    let id = done["result"]["outputs"]["diagnosis"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let diagnosis = cas.get_artifact(id).unwrap().payload;
    assert_eq!(diagnosis["occurrence_id"], selected["occurrence_id"]);
    assert_eq!(diagnosis["signature_id"], selected["signature_id"]);
    assert_eq!(diagnosis["source_sha"], selected["head_sha"]);
    assert_eq!(
        diagnosis["snapshot_id"],
        request["inputs"]["snapshot"][0]["artifact_id"]
    );
    let rows = accounting(&f, "ci-diagnose-one");
    assert_eq!(rows.len(), 2);
    let worker = rows
        .iter()
        .find(|r| r.reservation.node.ends_with(".work"))
        .unwrap();
    assert_eq!(worker.reservation.tokens, 20_000);
    assert_eq!(
        rows.iter().map(|r| r.reservation.tokens).sum::<u64>(),
        24_096
    );
    assert!(rows.iter().all(|r| r.charged_tokens == 5
        && r.unknown_usage.is_none()
        && r.settled_unix_ms.is_some()));
    let calls = f.calls();
    f.ok(&["task", "run", "ci-diagnose-one"]);
    assert_eq!(f.calls(), calls, "Accepted Task replay dispatches nothing");
}

#[test]
fn ci_health_distinct_release_head_reaches_task_selection_and_evidence_and_main_is_refused() {
    for (mode, accepted) in [("good", true), ("ci-main-revision", false)] {
        let f = Fixture::ci_health_release(false);
        std::fs::write(f.auth.join("mode"), mode).unwrap();
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
                .unwrap();
        let failure = &snapshot["failures"][0];
        assert_ne!(snapshot["source_sha"], failure["head_sha"]);
        assert_eq!(failure["release_blocking"], true);
        if accepted {
            let path = f.repo.join("ci-health/task.json");
            let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            task["requirements"]["source_sha"] = snapshot["source_sha"].clone();
            write(&path, &task);
            let refused = f.cli(&[
                "task",
                "plan",
                "--file",
                "ci-health/task.json",
                "--input-json",
                "snapshot=snapshot.json",
            ]);
            assert!(!refused.status.success(), "{}", text(&refused));
            assert!(
                text(&refused)
                    .contains("CI diagnosis selection does not name an exact snapshot occurrence"),
                "{}",
                text(&refused)
            );
            assert!(f.calls().is_empty());
            task["requirements"]["source_sha"] = failure["head_sha"].clone();
            write(&path, &task);
        }
        let planned = f.ci_plan();
        let (run, machine) = f.ci_scenario(&planned, &snapshot, &json!({}));
        assert_eq!(run["status"], "succeeded", "{mode}: {run:#}");
        let step = run["frames"]["root"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["step"] == "diagnose")
            .unwrap();
        let record: Value = serde_json::from_str(
            step["outputs"]["result"]["value"]["evidence"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(record["selection"]["source_sha"], failure["head_sha"]);
        assert_eq!(record["diagnosis"]["source_sha"], failure["head_sha"]);
        assert_eq!(
            record["diagnosis"]["evidence"][0]["source_sha"],
            failure["head_sha"]
        );
        assert_eq!(
            step["outputs"]["result"]["value"]["revision"],
            failure["head_sha"]
        );
        assert_eq!(
            record["provenance"]["kind"],
            if accepted {
                "accepted"
            } else {
                "deterministic_refusal"
            },
            "{mode}: {record:#}"
        );
        assert_eq!(
            record["receipt"]["result"]["acceptance"] == "satisfied",
            accepted
        );
        if !accepted {
            assert!(
                record["provenance"]["refusals"][0]["error_prefix"]
                    .as_str()
                    .unwrap()
                    .contains("CI diagnosis source mismatch")
            );
        }
        let request: Value = serde_json::from_str(
            std::fs::read_to_string(f.auth.join("requests"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            request["inputs"]["requirements"][0]["payload"]["specification"]["source_sha"],
            failure["head_sha"]
        );
        let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events[1]["phase"], "findings");
        assert_eq!(
            events[1]["release_alerts"],
            json!([failure["occurrence_id"]])
        );
        assert_eq!(events[1]["source_sha"], failure["head_sha"]);
        assert_eq!(f.calls(), "admission\nworker\n");
    }
}

#[test]
fn ci_health_native_reports_deterministic_remedy_and_settles_only_first_unresolved_occurrence() {
    for (classifier, known_index) in [
        ("known_transients", 0),
        ("infrastructure", 0),
        ("infrastructure", 1),
    ] {
        let f = Fixture::ci_health_release(true);
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
                .unwrap();
        let failures = snapshot["failures"].as_array().unwrap();
        assert_eq!(failures.len(), 2);
        assert_ne!(failures[0]["signature_id"], failures[1]["signature_id"]);
        let known = &failures[known_index];
        let selected = &failures[1 - known_index];
        let path = f.repo.join("ci-health/task.json");
        let mut task: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for field in ["occurrence_id", "signature_id"] {
            task["requirements"][field] = selected[field].clone();
        }
        task["requirements"]["source_sha"] = selected["head_sha"].clone();
        write(&path, &task);
        let planned = f.ci_plan();
        let (run, machine) = f.ci_scenario(
            &planned,
            &snapshot,
            &json!({classifier:[known["signature_id"]]}),
        );
        assert_eq!(run["status"], "succeeded", "{classifier}: {run:#}");
        assert_eq!(f.calls(), "admission\nworker\n");
        let steps = run["frames"]["root"]["steps"].as_array().unwrap();
        let diagnosis = steps.iter().find(|s| s["step"] == "diagnose").unwrap();
        let settlement: Value = serde_json::from_str(
            diagnosis["outputs"]["result"]["value"]["evidence"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            settlement["selection"]["occurrence_id"],
            selected["occurrence_id"]
        );
        assert_eq!(
            settlement["diagnosis"]["occurrence_id"],
            selected["occurrence_id"]
        );
        assert_eq!(settlement["diagnosis"]["source_sha"], selected["head_sha"]);
        assert_eq!(settlement["provenance"]["kind"], "accepted");
        assert_eq!(settlement["receipt"]["result"]["execution"], "completed");
        assert_eq!(settlement["receipt"]["result"]["acceptance"], "satisfied");
        let request: Value = serde_json::from_str(
            std::fs::read_to_string(f.auth.join("requests"))
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            request["inputs"]["requirements"][0]["payload"]["specification"]["occurrence_id"],
            selected["occurrence_id"]
        );
        assert_eq!(request["inputs"]["snapshot"][0]["payload"], snapshot);
        assert!(serde_json::to_vec(&request).unwrap().len().div_ceil(4) <= 20_000);
        let rows = accounting(&f, "ci-diagnose-one");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|a| a.charged_tokens == 5
            && a.unknown_usage.is_none()
            && a.settled_unix_ms.is_some()));
        let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .map(|e| e["phase"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "ci_observe_v1",
                "findings",
                "diagnosis",
                "remedy-intent",
                "outcome"
            ]
        );
        assert_eq!(events[1]["failures"].as_array().unwrap().len(), 2);
        assert_eq!(events[1]["unselected_occurrences"], json!([]));
        assert!(events[1]["unselected_reason"].is_null());
        crate::schemas::valid(
            &crate::schemas::validator("ci-health-notification-v5.json"),
            &events[1],
        );
        let results = events[1]["deterministic_results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        let diagnosis = &results[0]["diagnosis"];
        let category = if classifier == "infrastructure" {
            "ci_infra"
        } else {
            "transient"
        };
        assert_eq!(diagnosis["category"], category);
        assert_eq!(diagnosis["occurrence_id"], known["occurrence_id"]);
        assert_eq!(diagnosis["signature_id"], known["signature_id"]);
        assert_eq!(diagnosis["source_sha"], known["head_sha"]);
        assert_eq!(diagnosis["snapshot_id"], events[1]["snapshot_id"]);
        assert_eq!(
            diagnosis["source_artifact_ids"],
            json!([events[1]["snapshot_id"]])
        );
        let typed: af_contracts::ci_health::Diagnosis =
            serde_json::from_value(diagnosis.clone()).unwrap();
        typed
            .validate_against(
                events[1]["snapshot_id"].as_str().unwrap(),
                &serde_json::from_value(snapshot.clone()).unwrap(),
            )
            .unwrap();
        let (row, intended) = if known["release_blocking"] == true {
            (2, "release_alert")
        } else if classifier == "infrastructure" {
            (3, "infrastructure_alert")
        } else {
            (4, "rerun_intended")
        };
        assert_eq!(results[0]["row"], row);
        assert_eq!(results[0]["intended"], intended);
        assert_eq!(events[1]["report_only"], true);
        assert_eq!(events[2]["occurrence_id"], selected["occurrence_id"]);
        assert_eq!(events[2]["category"], "product_bug");
        assert_eq!(events[3]["occurrence_id"], selected["occurrence_id"]);
        assert_eq!(
            events[3]["row"],
            if selected["release_blocking"] == true {
                2
            } else {
                7
            }
        );
        assert_eq!(
            events[3]["intended"],
            if selected["release_blocking"] == true {
                "release_alert"
            } else {
                "report_only"
            }
        );
        let calls = f.calls();
        assert_eq!(f.ok(&["scenario", "resume", "ci-health-fixture"]), run);
        assert_eq!(f.calls(), calls);
    }
}

#[test]
fn ci_health_native_reports_every_deterministic_diagnosis_and_first_remedy_without_a_task() {
    let f = Fixture::ci_health_release(true);
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    let failures = snapshot["failures"].as_array().unwrap();
    let planned = f.ci_plan();
    let (run, machine) = f.ci_scenario(
        &planned,
        &snapshot,
        &json!({"infrastructure":[failures[0]["signature_id"],failures[1]["signature_id"]],
            "known_transients":[failures[1]["signature_id"]],"small_repair":true}),
    );
    assert_eq!(run["status"], "succeeded", "{run:#}");
    assert!(
        f.calls().is_empty(),
        "All classifications must bypass the Provider"
    );
    assert_eq!(run["dispatches"], 4);
    assert!(
        run["frames"]["root"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["step"] != "diagnose")
    );
    let recorded = std::fs::read_to_string(machine.join("events")).unwrap();
    let events: Vec<Value> = recorded
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|e| e["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci_observe_v1", "findings", "remedy-intent", "outcome"]
    );
    let findings = &events[1];
    crate::schemas::valid(
        &crate::schemas::validator("ci-health-notification-v5.json"),
        findings,
    );
    assert_eq!(findings["unselected_occurrences"], json!([]));
    let results = findings["deterministic_results"].as_array().unwrap();
    assert_eq!(results.len(), failures.len());
    for (result, failure) in results.iter().zip(failures) {
        let diagnosis = &result["diagnosis"];
        assert_eq!(
            diagnosis["category"], "ci_infra",
            "infrastructure precedes overlapping transient classification"
        );
        assert_eq!(diagnosis["occurrence_id"], failure["occurrence_id"]);
        assert_eq!(diagnosis["signature_id"], failure["signature_id"]);
        assert_eq!(diagnosis["source_sha"], failure["head_sha"]);
        let typed: af_contracts::ci_health::Diagnosis =
            serde_json::from_value(diagnosis.clone()).unwrap();
        typed
            .validate_against(
                findings["snapshot_id"].as_str().unwrap(),
                &serde_json::from_value(snapshot.clone()).unwrap(),
            )
            .unwrap();
        let (row, intended) = if failure["release_blocking"] == true {
            (2, "release_alert")
        } else {
            (3, "infrastructure_alert")
        };
        assert_eq!(result["row"], row);
        assert_eq!(result["intended"], intended);
    }
    assert_eq!(
        findings["release_alerts"],
        json!([failures[1]["occurrence_id"]])
    );
    assert_eq!(findings["report_only"], true);
    assert_eq!(f.ok(&["scenario", "resume", "ci-health-fixture"]), run);
    assert_eq!(
        std::fs::read_to_string(machine.join("events")).unwrap(),
        recorded
    );
    assert!(f.calls().is_empty());
}

#[test]
fn ci_health_large_scope_completes_native_findings_diagnosis_remedy_and_outcome() {
    let home = crate::common::private_home_env()
        .into_iter()
        .find(|(name, _)| *name == "HOME")
        .unwrap()
        .1;
    let scratch = tempfile::tempdir_in(home).unwrap();
    let output = std::process::Command::new("python3")
        .args(["-I", "-c", r#"
import importlib.util,json,pathlib,sys
sys.dont_write_bytecode=True
root=pathlib.Path(sys.argv[1]); directory=pathlib.Path(sys.argv[2])
spec=importlib.util.spec_from_file_location('ci',root/'scripts/ci-health.py')
c=importlib.util.module_from_spec(spec);spec.loader.exec_module(c)
service=json.loads((root/'fixtures/scenarios/maintenance-ci-health/failing-service.json').read_text())
service['repository']='r'*40000+'/project'
journal=c.Journal(directory/'observer.sqlite')
try: snapshot=c.observe(service,journal)
finally: journal.close()
c.validate_snapshot(snapshot)
assert snapshot['complete'] and len(snapshot['failures'])==1
(directory/'snapshot.json').write_bytes(c.canonical(snapshot))
print(json.dumps({'observation_service':service}))
"#])
        .arg(crate::task_report::workspace())
        .arg(scratch.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    // Exercise both deterministic and native Task settlement paths through the installed
    // adapter. Its unchanged 65,536-byte input bound checks the real serialized requests.
    for deterministic in [true, false] {
        let f = Fixture::ci_health_from_snapshot(&scratch.path().join("snapshot.json"));
        std::fs::write(f.root.path().join("ci-service.json"), &output.stdout).unwrap();
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
                .unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("ci-health-snapshot-v1.json"),
            &snapshot,
        );
        assert!(serde_json::to_vec(&snapshot).unwrap().len() < 65536);
        assert_eq!(snapshot["repository"].as_str().unwrap().len(), 40008);
        let failure = &snapshot["failures"][0];
        let options = if deterministic {
            json!({"infrastructure":[failure["signature_id"]]})
        } else {
            json!({"small_repair":true})
        };
        let planned = f.ci_plan();
        let (run, machine) = f.ci_scenario(&planned, &snapshot, &options);
        assert_eq!(run["status"], "succeeded", "{run:#}");
        let recorded = std::fs::read_to_string(machine.join("events")).unwrap();
        let events: Vec<Value> = recorded
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let expected = if deterministic {
            vec!["ci_observe_v1", "findings", "remedy-intent", "outcome"]
        } else {
            vec![
                "ci_observe_v1",
                "findings",
                "diagnosis",
                "remedy-intent",
                "outcome",
            ]
        };
        assert_eq!(
            events
                .iter()
                .map(|e| e["phase"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(run["dispatches"], if deterministic { 4 } else { 6 });
        let findings = &events[1];
        assert_eq!(findings["failures"].as_array().unwrap().len(), 1);
        assert_eq!(
            findings["failures"][0]["occurrence_id"],
            failure["occurrence_id"]
        );
        assert_eq!(
            findings["deterministic_results"].as_array().unwrap().len(),
            usize::from(deterministic)
        );
        for event in &events[1..] {
            crate::schemas::valid(
                &crate::schemas::validator("ci-health-notification-v5.json"),
                event,
            );
            for key in ["repository", "source_sha"] {
                assert_eq!(event[key], snapshot[key]);
            }
            for key in ["occurrence_id", "signature_id"] {
                assert_eq!(event[key], failure[key]);
            }
            assert_eq!(event["snapshot_id"], findings["snapshot_id"]);
            assert_eq!(event["scenario_run"], "ci-health-fixture");
            assert_eq!(event["report_only"], true);
        }
        let remedy = &events[events.len() - 2];
        assert_eq!(remedy["row"], if deterministic { 3 } else { 6 });
        assert_eq!(
            remedy["intended"],
            if deterministic {
                "infrastructure_alert"
            } else {
                "repair_intended"
            }
        );
        assert_eq!(
            remedy["category"],
            if deterministic {
                "ci_infra"
            } else {
                "product_bug"
            }
        );
        if deterministic {
            let diagnosis: af_contracts::ci_health::Diagnosis =
                serde_json::from_value(remedy["deterministic_diagnosis"].clone()).unwrap();
            diagnosis
                .validate_against(
                    findings["snapshot_id"].as_str().unwrap(),
                    &serde_json::from_value(snapshot).unwrap(),
                )
                .unwrap();
            assert_eq!(
                findings["deterministic_results"][0]["diagnosis"],
                remedy["deterministic_diagnosis"]
            );
        }
        let calls = f.calls();
        assert_eq!(calls.is_empty(), deterministic);
        assert_eq!(f.ok(&["scenario", "resume", "ci-health-fixture"]), run);
        assert_eq!(
            std::fs::read_to_string(machine.join("events")).unwrap(),
            recorded
        );
        assert_eq!(f.calls(), calls);
    }
}

#[test]
fn ci_health_installed_production_adapter_bounds_wire_evidence_and_scenario_reports_retained_findings()
 {
    let home = crate::common::private_home_env()
        .into_iter()
        .find(|(name, _)| *name == "HOME")
        .unwrap()
        .1;
    let scratch = tempfile::tempdir_in(home).unwrap();
    let output = std::process::Command::new("python3")
        .args(["-I", "-c", r#"
import importlib.util,pathlib,sys
sys.dont_write_bytecode=True
root=pathlib.Path(sys.argv[1]); directory=pathlib.Path(sys.argv[2])
spec=importlib.util.spec_from_file_location('ci_test',root/'scripts/test-ci-health.py')
t=importlib.util.module_from_spec(spec);sys.modules[spec.name]=t;spec.loader.exec_module(t)
t.ProductionTests.setUpClass(); case=t.ProductionTests(); case.setUp()
options={**case.options,'state':str(directory/'observer.sqlite'),'max_requests':32}
receipt=directory/'publish.json'; receipt.write_bytes(case.receipt.read_bytes())
options['nightly_receipt']=str(receipt)
routes=t.near_limit_routes(); snapshot=case.P.observe(options,opener=t.Transport(routes))
assert not snapshot['complete'] and snapshot['failures']
assert '64 KiB adapter response bound exceeded' in snapshot['omissions']
(directory/'snapshot.json').write_bytes(t.C.canonical(snapshot))
occurrences=directory/'occurrences'; occurrences.mkdir(mode=0o700)
case.P.stage_occurrence(snapshot,since=options['since'],nightly_receipt=receipt,occurrence_dir=occurrences)
staged={k:v for k,v in options.items() if k not in ('since','captured_at','nightly_receipt','release_pr')}
staged.update(occurrence_dir=str(occurrences),destination='fixture:ci-health')
spec=importlib.util.spec_from_file_location('ci_config',t.FIXTURE/'configure.py')
compiler=importlib.util.module_from_spec(spec);spec.loader.exec_module(compiler)
program=t.hermetic_production_adapter(case.P,staged,routes,compiler.adapter_bytes(root))
(directory/'adapter').write_bytes(program)
case.doCleanups()
"#])
        .arg(crate::task_report::workspace())
        .arg(scratch.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let f = Fixture::ci_health_from_snapshot(&scratch.path().join("snapshot.json"));
    std::fs::copy(
        scratch.path().join("adapter"),
        f.root.path().join("ci-production-adapter"),
    )
    .unwrap();
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    let planned = f.ci_plan();
    let (run, machine) = f.ci_scenario(&planned, &snapshot, &json!({}));
    assert_eq!(run["status"], "succeeded", "{run:#}");
    assert!(
        f.calls().is_empty(),
        "Incomplete evidence cannot dispatch a Provider"
    );
    let observe = &run["frames"]["root"]["steps"][0]["outputs"]["result"]["value"];
    assert_eq!(observe["satisfied"], true);
    let captured: Value = serde_json::from_str(observe["evidence"].as_str().unwrap()).unwrap();
    assert_eq!(captured, snapshot);
    crate::schemas::valid(
        &crate::schemas::validator("ci-health-snapshot-v1.json"),
        &captured,
    );
    assert_eq!(captured["complete"], false);
    assert!(
        captured["omissions"]
            .as_array()
            .unwrap()
            .contains(&json!("64 KiB adapter response bound exceeded"))
    );
    let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let findings: Vec<&Value> = events.iter().filter(|e| e["phase"] == "findings").collect();
    assert!(!findings.is_empty());
    let retained: Vec<&Value> = findings
        .iter()
        .flat_map(|e| e["failures"].as_array().unwrap())
        .collect();
    assert_eq!(
        retained.len(),
        captured["failures"].as_array().unwrap().len()
    );
    assert!(!retained.is_empty());
    for (actual, expected) in retained
        .iter()
        .zip(captured["failures"].as_array().unwrap())
    {
        for key in [
            "occurrence_id",
            "head_sha",
            "job_name",
            "log_artifacts",
            "release_blocking",
        ] {
            assert_eq!(actual[key], expected[key], "{key}");
        }
    }
    let alerts: Vec<&Value> = findings
        .iter()
        .flat_map(|e| e["release_alerts"].as_array().unwrap())
        .collect();
    assert_eq!(
        alerts,
        retained
            .iter()
            .map(|f| &f["occurrence_id"])
            .collect::<Vec<_>>()
    );
    for event in &events {
        crate::schemas::valid(
            &crate::schemas::validator("ci-health-notification-v5.json"),
            event,
        );
    }
    assert_eq!(events.last().unwrap()["phase"], "outcome");
    assert_eq!(f.ok(&["scenario", "resume", "ci-health-fixture"]), run);
}

#[test]
fn ci_health_native_findings_alert_unselected_release_failure_even_when_diagnosis_is_exhausted() {
    let f = Fixture::ci_health_release(true);
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    let failures = snapshot["failures"].as_array().unwrap();
    assert_eq!(failures.len(), 2);
    assert_eq!(failures[0]["release_blocking"], false);
    assert_eq!(failures[1]["release_blocking"], true);
    let planned = f.ci_plan();
    let (run, machine) = f.ci_scenario(&planned, &snapshot, &json!({"exhausted":true}));
    assert_eq!(run["status"], "succeeded", "{run:#}");
    assert!(
        f.calls().is_empty(),
        "An exhausted reservation must not invoke a Provider"
    );
    let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[1]["phase"], "findings");
    let findings = &events[1];
    crate::schemas::valid(
        &crate::schemas::validator("ci-health-notification-v5.json"),
        findings,
    );
    assert_eq!(findings["failures"].as_array().unwrap().len(), 2);
    let release = &findings["failures"][1];
    for key in [
        "occurrence_id",
        "signature_id",
        "workflow_id",
        "workflow_file",
        "run_id",
        "run_attempt",
        "job_id",
        "job_name",
        "platform",
        "conclusion",
        "head_sha",
        "log_artifacts",
        "release_blocking",
    ] {
        assert_eq!(release[key], failures[1][key], "{key}");
    }
    assert_eq!(
        findings["release_alerts"],
        json!([failures[1]["occurrence_id"]])
    );
    assert_eq!(
        findings["unselected_occurrences"],
        json!([failures[1]["occurrence_id"]])
    );
    assert_eq!(
        findings["unselected_reason"],
        "diagnosis_reservation_exhausted"
    );
    assert_eq!(events[2]["phase"], "remedy-intent");
    assert_eq!(events[3]["phase"], "outcome");
    let resumed = f.ok(&["scenario", "resume", "ci-health-fixture"]);
    assert_eq!(resumed, run);
    assert_eq!(
        std::fs::read_to_string(machine.join("events"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}

#[test]
fn ci_health_native_scenario_notifies_findings_and_first_remedy_before_outcome_and_open_pr_watch() {
    let f = Fixture::ci_health();
    std::fs::write(f.auth.join("mode"), "ci-test").unwrap();
    let plan = f.ci_plan();
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    let (run, machine) = f.ci_scenario(
        &plan,
        &snapshot,
        &json!({"gate_evidence":serde_json::from_slice::<Value>(&std::fs::read(crate::task_report::workspace()
                .join("fixtures/scenarios/maintenance-ci-health/gate-evidence.json")).unwrap()).unwrap(),
                "small_repair":true,"watch":true}),
    );
    assert_eq!(run["status"], "waiting", "{run:#}");
    assert_eq!(run["waiting"]["kind"], "condition", "{run:#}");
    assert_eq!(run["waiting"]["step"], "root/watch", "{run:#}");
    let read_events = || -> Vec<Value> {
        std::fs::read_to_string(machine.join("events"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    };
    let events = read_events();
    assert_eq!(
        events
            .iter()
            .map(|e| e["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "ci_observe_v1",
            "findings",
            "diagnosis",
            "remedy-intent",
            "outcome",
            "merged"
        ]
    );
    assert_eq!(
        events[3]["row"], 5,
        "eligible quarantine evidence precedes repair"
    );
    assert_eq!(events[3]["intended"], "quarantine_evidence_intended");
    assert_eq!(run["frames"]["root"]["steps"].as_array().unwrap().iter()
        .filter(|s|s["step"].as_str().unwrap().starts_with("remedy-") && s["skipped"].is_null()).count(),1);
    for event in &events[1..5] {
        assert_eq!(event["scenario_run"], "ci-health-fixture");
        assert!(af_contracts::is_digest(
            event["snapshot_id"].as_str().unwrap()
        ));
        assert_eq!(event["report_only"], true);
    }
    let calls = f.calls();
    let resumed = f.ok(&["scenario", "resume", "ci-health-fixture"]);
    assert_eq!(resumed["status"], "waiting");
    assert_eq!(
        f.calls(),
        calls,
        "Watch/resume must not redispatch diagnosis"
    );
    assert_eq!(
        read_events().len(),
        events.len() + 1,
        "only the bounded merge observation repeats"
    );
    let mut service: Value =
        serde_json::from_slice(&std::fs::read(machine.join("service.json")).unwrap()).unwrap();
    service["merged"] = json!(true);
    write(&machine.join("service.json"), &service);
    let done = f.ok(&["scenario", "resume", "ci-health-fixture"]);
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        f.calls(),
        calls,
        "Merged Watch must not redispatch diagnosis"
    );
    let mut expected_events = events.clone();
    expected_events.extend([json!({"phase":"merged"}), json!({"phase":"merged"})]);
    assert_eq!(
        read_events(),
        expected_events,
        "Each resume adds exactly one merge observation, preserving all pre-Watch notifications"
    );
    let before = run["frames"]["root"]["steps"].as_array().unwrap();
    let after = done["frames"]["root"]["steps"].as_array().unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(
        &after[..after.len() - 1],
        &before[..before.len() - 1],
        "Every pre-Watch step retains its exact progress, intents and outputs"
    );
    let watch_before = before.last().unwrap();
    let watch_after = after.last().unwrap();
    assert_eq!(watch_after["step"], "watch");
    assert_eq!(watch_before["intents"].as_array().unwrap().len(), 1);
    assert_eq!(watch_after["intents"].as_array().unwrap().len(), 1);
    assert_eq!(
        watch_after["intents"][0]["id"], watch_before["intents"][0]["id"],
        "Watch observations reconcile the same durable intent"
    );
    assert_eq!(
        done["dispatches"].as_u64().unwrap(),
        run["dispatches"].as_u64().unwrap().checked_add(2).unwrap(),
        "The unsatisfied and merged observations each consume one dispatch while reusing the Wait intent"
    );
}

#[test]
fn ci_health_refused_diagnosis_records_deterministic_unknown_and_notifies_without_remedy_or_watch()
{
    for mode in [
        "ci-foreign-evidence",
        "ci-malformed-evidence",
        "ci-foreign-signature",
        "ci-foreign-occurrence",
    ] {
        let f = Fixture::ci_health();
        std::fs::write(f.auth.join("mode"), mode).unwrap();
        let plan = f.ci_plan();
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
                .unwrap();
        // Even explicitly requested fixture repair/Watch paths must lose to the refusal.
        let (run, machine) =
            f.ci_scenario(&plan, &snapshot, &json!({"small_repair":true,"watch":true}));
        assert_eq!(run["status"], "succeeded", "{mode}: {run:#}");
        let steps = run["frames"]["root"]["steps"].as_array().unwrap();
        assert_eq!(
            steps
                .iter()
                .filter(|s| s["skipped"].is_null())
                .map(|s| s["step"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "observe",
                "notify-findings",
                "diagnose",
                "notify-diagnosis",
                "remedy-1",
                "report-outcome"
            ],
            "only observation, diagnosis and report-only notifications execute"
        );
        assert_eq!(
            run["dispatches"], 6,
            "no retry or additional effect dispatch"
        );
        let diagnosis = steps.iter().find(|s| s["step"] == "diagnose").unwrap();
        assert_eq!(diagnosis["outputs"]["result"]["type"], "observation");
        let observation = &diagnosis["outputs"]["result"]["value"];
        let record: Value =
            serde_json::from_str(observation["evidence"].as_str().unwrap()).unwrap();
        crate::schemas::valid(
            &crate::schemas::validator("scenario-ci-diagnosis-v2.json"),
            &record,
        );
        assert_eq!(record["provenance"]["kind"], "deterministic_refusal");
        assert_eq!(record["diagnosis"]["category"], "unknown");
        assert_eq!(
            record["diagnosis"]["rationale"],
            "Deterministic unknown: native diagnosis Task output was refused"
        );
        assert_ne!(record["receipt"]["result"]["acceptance"], "satisfied");
        assert!(
            record["receipt"]["result"]["outputs"]
                .get("validation")
                .is_none()
        );
        assert_eq!(record["receipt"]["plan_id"], plan["plan_id"]);
        assert_eq!(record["receipt"]["revision_id"], plan["revision_id"]);
        assert_eq!(record["diagnosis"]["snapshot_id"], record["snapshot_id"]);
        assert_eq!(
            record["diagnosis"]["source_artifact_ids"],
            json!([record["snapshot_id"]])
        );
        for key in ["occurrence_id", "signature_id"] {
            assert_eq!(record["diagnosis"][key], snapshot["failures"][0][key]);
            assert_eq!(record["selection"][key], record["diagnosis"][key]);
        }
        assert_eq!(record["diagnosis"]["source_sha"], snapshot["source_sha"]);
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let native_result = cas
            .get_artifact(record["receipt"]["result_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            native_result.payload, record["receipt"]["result"],
            "original refusal receipt retained"
        );
        let worker = accounting(&f, "ci-diagnose-one")
            .into_iter()
            .find(|a| a.reservation.node.ends_with(".work"))
            .unwrap();
        let refusals = record["provenance"]["refusals"].as_array().unwrap();
        assert_eq!(refusals.len(), 1);
        assert_eq!(refusals[0]["attempt_id"], worker.attempt_id);
        assert_eq!(
            worker.attempt_id.len(),
            26,
            "native Attempt identity retained"
        );
        assert!(!af_contracts::is_digest(&worker.attempt_id));
        assert!(
            record["receipt"]["result"]["outputs"]
                .get("diagnosis")
                .is_none()
        );
        let diagnostic = cas
            .get_json(refusals[0]["diagnostic_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            refusals[0]["error_prefix"],
            diagnostic["error"]
                .as_str()
                .unwrap()
                .chars()
                .take(2048)
                .collect::<String>()
        );
        let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .map(|e| e["phase"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "ci_observe_v1",
                "findings",
                "diagnosis",
                "remedy-intent",
                "outcome"
            ]
        );
        assert!(events[1]["category"].is_null());
        assert!(events[1]["deterministic_diagnosis"].is_null());
        assert_eq!(
            events[1]["failures"][0]["occurrence_id"],
            snapshot["failures"][0]["occurrence_id"]
        );
        for event in &events[2..] {
            assert_eq!(event["category"], "unknown");
            assert_eq!(event["deterministic_diagnosis"], record["diagnosis"]);
            assert_eq!(event["snapshot_id"], record["snapshot_id"]);
            assert_eq!(event["report_only"], true);
        }
        assert_eq!(events[3]["row"], 1);
        assert_eq!(events[3]["intended"], "alert_only");
        assert_eq!(
            steps
                .iter()
                .filter(|s| s["step"].as_str().unwrap().starts_with("remedy-")
                    && s["skipped"].is_null())
                .count(),
            1
        );
        assert!(steps.iter().find(|s| s["step"] == "watch").unwrap()["skipped"].is_string());
        assert_eq!(
            f.calls(),
            "admission\nworker\n",
            "no repair or second diagnosis"
        );
        assert_eq!(run["task_charges"].as_object().unwrap().len(), 1);
        let charged: u64 = accounting(&f, "ci-diagnose-one")
            .iter()
            .map(|a| u64::try_from(a.charged_tokens).unwrap())
            .sum();
        assert_eq!(record["receipt"]["charged_tokens"], charged);
        assert_eq!(
            run["task_charges"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap()["charged"],
            charged
        );
        let resumed = f.ok(&["scenario", "resume", "ci-health-fixture"]);
        assert_eq!(resumed, run, "terminal replay retains exact native refusal");
        assert_eq!(f.calls(), "admission\nworker\n");
        assert_eq!(
            std::fs::read_to_string(machine.join("events"))
                .unwrap()
                .lines()
                .count(),
            5
        );
    }
}

#[test]
fn ci_health_native_acceptance_refuses_foreign_diagnosis_evidence_and_identity() {
    for (mode, reason) in [
        (
            "ci-foreign-signature",
            "CI diagnosis references evidence outside selected snapshot occurrence",
        ),
        (
            "ci-foreign-occurrence",
            "CI diagnosis occurrence absent from snapshot",
        ),
        (
            "ci-foreign-evidence",
            "CI diagnosis references evidence outside selected snapshot occurrence",
        ),
    ] {
        let f = Fixture::ci_health();
        std::fs::write(f.auth.join("mode"), mode).unwrap();
        let planned = f.ci_plan();
        let run = f.cli(&[
            "task",
            "run",
            "ci-diagnose-one",
            "--confirm-plan",
            planned["plan_id"].as_str().unwrap(),
        ]);
        let shown: Value = serde_json::from_slice(&run.stdout).unwrap();
        assert!(
            shown["result"].is_object(),
            "{mode}: diagnosis must execute before its refusal is checked: {}",
            text(&run)
        );
        assert_ne!(
            shown["result"]["acceptance"], "satisfied",
            "{mode}: {shown:#}"
        );
        assert!(!run.status.success(), "{mode}: {}", text(&run));
        assert!(shown["result"]["outputs"].get("validation").is_none());
        assert_eq!(f.calls(), "admission\nworker\n", "{mode}: {shown:#}");
        let rows = accounting(&f, "ci-diagnose-one");
        let worker = rows
            .iter()
            .find(|r| r.reservation.node.ends_with(".work"))
            .unwrap();
        let Some(af_contracts::task::execution::TaskAttemptResultV1::Failed {
            diagnostic_id, ..
        }) = &worker.result
        else {
            panic!("Foreign CI evidence must be refused at Worker admission: {mode}");
        };
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let diagnostic = cas.get_json(diagnostic_id).unwrap();
        assert!(
            diagnostic["error"].as_str().unwrap().contains(reason),
            "{mode}: {diagnostic:#}"
        );
        assert_eq!(worker.charged_tokens, 5);
    }
}

#[test]
fn ci_health_incomplete_snapshot_reports_without_a_task_remedy_or_watch() {
    let f = Fixture::ci_health();
    let mut snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    snapshot["complete"] = json!(false);
    snapshot["omissions"] = json!(["fixture unreadable logs"]);
    write(&f.repo.join("ci-health/snapshot.json"), &snapshot);
    let plan = f.ci_plan();
    let (run, machine) = f.ci_scenario(&plan, &snapshot, &json!({}));
    assert_eq!(run["status"], "succeeded", "{run:#}");
    assert!(
        !f.calls().contains("worker"),
        "No diagnosis model for incomplete evidence"
    );
    let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .map(|e| e["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci_observe_v1", "findings", "remedy-intent", "outcome"]
    );
    assert!(run["task_charges"].as_object().unwrap().is_empty());
}

#[test]
fn ci_health_same_main_changed_job_evidence_stops_before_diagnosis_and_notifications() {
    let f = Fixture::ci_health();
    let plan = f.ci_plan();
    let snapshot: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("ci-health/snapshot.json")).unwrap())
            .unwrap();
    let source = crate::task_report::workspace().join("fixtures/scenarios/maintenance-ci-health");
    let mut github: Value =
        serde_json::from_slice(&std::fs::read(source.join("github-service.json")).unwrap())
            .unwrap();
    let job = &mut github["routes"]["repos/octo/project/actions/runs/101/attempts/1/jobs?per_page=100"]
        ["jobs"][0];
    job["failing_tests"] = json!(["a_different_failed_test"]);
    assert_eq!(
        github["routes"]["repos/octo/project/commits/main"]["sha"],
        snapshot["source_sha"]
    );
    write(
        &f.root.path().join("ci-service.json"),
        &json!({"observation_service":serde_json::from_slice::<Value>(
            &std::fs::read(source.join("failing-service.json")).unwrap()).unwrap(),
            "github_service":github}),
    );
    let (run, machine) = f.ci_scenario(&plan, &snapshot, &json!({}));
    assert_eq!(run["status"], "failed", "{run:#}");
    assert!(run["task_charges"].as_object().unwrap().is_empty());
    assert!(
        f.calls().is_empty(),
        "No Provider admission or diagnosis for stale evidence"
    );
    let events: Vec<Value> = std::fs::read_to_string(machine.join("events"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events, vec![json!({"phase":"ci_observe_v1"})]);
}

#[test]
fn ci_health_malformed_snapshot_identity_is_refused_before_provider_admission() {
    let f = Fixture::ci_health();
    let path = f.repo.join("ci-health/snapshot.json");
    let mut snapshot: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    snapshot["failures"][0]["run_attempt"] = json!(2);
    write(&path, &snapshot);
    let output = f.cli(&[
        "task",
        "plan",
        "--file",
        "ci-health/task.json",
        "--input-json",
        "snapshot=snapshot.json",
    ]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("CI occurrence or signature identity mismatch"),
        "{}",
        text(&output)
    );
    assert!(
        f.calls().is_empty(),
        "Malformed snapshot must not spend Provider tokens"
    );
}

#[test]
fn ci_health_fixture_catalog_pins_the_exact_native_package_digest_output() {
    let root = crate::task_report::workspace();
    let fixture = root.join("fixtures/scenarios/maintenance-ci-health/task");
    let catalog: Value =
        toml::from_str(&std::fs::read_to_string(fixture.join("catalog.toml")).unwrap()).unwrap();
    for part in ["kind", "pipeline", "worker"] {
        let name = format!("ci/diagnosis-{part}");
        let path = root.join(catalog["packages"][&name]["path"].as_str().unwrap());
        let output = crate::common::af()
            .current_dir(&root)
            .args(["catalog", "digest"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        let digest = String::from_utf8(output.stdout).unwrap();
        println!("{name}: {}", digest.trim());
        assert_eq!(
            digest.trim(),
            catalog["packages"][&name]["digest"].as_str().unwrap()
        );
        assert_eq!(
            digest.trim(),
            af_pipeline_config::lock::package_digest(&name, &path).unwrap()
        );
    }
    assert_eq!(catalog["kinds"]["ci-diagnosis"], "ci/diagnosis-kind");
}

#[test]
fn ci_health_fixture_allows_snapshot_mutation_from_a_sealed_source() {
    let directory = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(directory.path().join("cas")).unwrap();
    let bytes = std::fs::read(
        crate::task_report::workspace()
            .join("fixtures/scenarios/maintenance-ci-health/snapshot.json"),
    )
    .unwrap();
    let manifest = af_source_git::Manifest::new(vec![af_source_git::Entry {
        path: "snapshot.json".into(),
        kind: af_source_git::EntryKind::File,
        content: cas.put(&bytes).unwrap(),
        size: bytes.len() as u64,
    }])
    .unwrap();
    let sandbox =
        af_sandbox::Sandbox::materialize(&manifest, &cas, af_sandbox::Mode::ReadOnly).unwrap();
    let source = sandbox.root().join("snapshot.json");
    let original_mode = std::fs::metadata(&source).unwrap().permissions().mode();
    assert_eq!(original_mode & 0o777, 0o444);

    let fixture = Fixture::ci_health_from_snapshot(&source);
    let copy = fixture.repo.join("ci-health/snapshot.json");
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert_ne!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o200,
        0
    );
    let mut snapshot: Value = serde_json::from_slice(&bytes).unwrap();
    snapshot["complete"] = json!(false);
    write(&copy, &snapshot);
    assert_ne!(std::fs::read(copy).unwrap(), bytes);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(source).unwrap().permissions().mode(),
        original_mode
    );
    assert!(sandbox.seal().unwrap().unchanged());
    assert!(fixture.calls().is_empty());
}
