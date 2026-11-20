use crate::{
    schemas,
    task_cli::copy_tree,
    task_report::{git, text, workspace},
};
use af_store::Cas;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Output,
};

const ARTIFACT: &str = "fixtures/issue-triage/scenario.json";
const STAGED: &str = "fixtures/issue-triage";
const FAKE_CODEX: &str = r#"#!/usr/bin/python3 -B
import json, os, re, sys

if sys.argv[1:2] == ['app-server']:
    for line in sys.stdin:
        r = json.loads(line)
        if r.get('id') == 1:
            print(json.dumps({'id':1,'result':{}}), flush=True)
        if r.get('id') == 2:
            print(json.dumps({'id':2,'result':{'account':{
                'type':'chatgpt','email':'portable@example.invalid'}}}), flush=True)
    sys.exit(0)

request = sys.stdin.read()
stage = 'admission'
if request == PROBE_INPUT:
    message = 'OK'
else:
    inputs = {k:v[0] for k,v in json.loads(request)['inputs'].items()}
    sources = inputs['sources']['payload']['sources']
    issue = sources['issue']
    number = int(re.search(r'Issue #(\d+)', issue['title']).group(1))
    schema = json.loads(sources['issue-triage']['text'])
    assert schema['properties']['schema']['const'] == 'af/issue-triage@1'
    triage = {'schema':'af/issue-triage@1','issue':number,
              'body_sha256':issue['revision'].split(':')[1],
              'class':'docs','size':'M','pipeline':'default','adr':None,'confidence':0.9,
              'rationale':issue['title'],
              'resolution':{'state':'open','evidence':[],'addressed':[],'remaining':[]},
              'duplicates':[],'needs_info':[],'acceptance':'The contribution guide links the quickstart.'}
    block = '```json\n' + json.dumps(triage) + '\n```'
    if 'document' in inputs:
        stage = 'verifier'
        assert inputs['checks']['payload']['outcome'] == 'passed'
        assert issue['title'].split(': ', 1)[1] in inputs['document']['payload']['text']
        outputs = {'result':[{'document_id':inputs['document']['artifact_id'],
            'sources_id':inputs['sources']['artifact_id'],
            'requirements_id':inputs['requirements']['artifact_id'],
            'check_receipt_id':inputs['checks']['artifact_id'],
            'source_snapshot_id':inputs['source']['snapshot_id'],
            'outcome':'passed','summary':'Independent triage.\n\n'+block}]}
    else:
        stage = 'author'
        outputs = {'draft':[{'schema':'af.document-draft/3','title':issue['title'],
            'sections':[{'heading':'Triage','body':block},
                {'heading':'Findings','body':issue['text']},
                {'heading':'Recommendation','body':'Update the guide.'}],
            'citations':[{'kind':'source','name':'issue'},
                {'kind':'repository','location':{'path':'CONTRIBUTING.md','line':1}}]}]}
    message = json.dumps({'schema':'af.worker-reply/1','outputs':outputs})
with open(os.path.join(os.environ['CODEX_HOME'],'calls'), 'a') as calls:
    calls.write(stage+'\n')
if '-o' in sys.argv:
    with open(sys.argv[sys.argv.index('-o')+1], 'w') as reply:
        reply.write(message)
print(json.dumps({'type':'thread.started','thread_id':'portable-'+stage}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':2,'cached_input_tokens':0,'output_tokens':1}}))
"#;

fn save(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn capture(repo: &Path) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "portable Scenario fixtures"]);
}

struct Receiver {
    root: PathBuf,
    repo: PathBuf,
    input: PathBuf,
    tasks: PathBuf,
    scenarios: PathBuf,
    path: std::ffi::OsString,
    provider_home: PathBuf,
}

impl Receiver {
    fn new(root: PathBuf, issue: u64) -> Self {
        fs::create_dir(&root).unwrap();
        let repo = root.join("arbitrary checkout");
        fs::create_dir_all(repo.join(".af")).unwrap();
        copy_tree(&workspace().join(STAGED), &repo.join(STAGED));
        fs::write(repo.join("CONTRIBUTING.md"), "# Contribution evidence\n").unwrap();
        fs::copy(
            workspace().join(".af/report-policy.toml"),
            repo.join(".af/report-policy.toml"),
        )
        .unwrap();
        // Receiver policy and Provider mapping alone. There is no installed triage package.
        fs::write(
            repo.join(".af/task-catalog.toml"),
            format!(
                r#"schema = "af.task-catalog/2"
report_policy = ".af/report-policy.toml"
packages = {{}}

[independence]
command_workers_by_package = true
distinct_principals = false
distinct_providers = false
distinct_models = true

[providers]
"backlog/triage-author" = "portable-{issue}"
"backlog/triage-verifier" = "unused-receiver-default"
"#,
            ),
        )
        .unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        capture(&repo);

        let input = root.join("issue.json");
        let mut supplied = read(&repo.join(STAGED).join("example/portable-inputs.json"));
        for field in ["title", "uri", "text"] {
            let value = &mut supplied["sources"]["issue"][field];
            *value = value
                .as_str()
                .unwrap()
                .replace("101", &issue.to_string())
                .into();
        }
        if issue != 101 {
            let source = &mut supplied["sources"]["issue"];
            let extra = "\nAlso link the supported build prerequisites.";
            let issue_text = source["text"].as_str().unwrap();
            let body = issue_text.split_once("--- body ---\n").unwrap().1;
            let revision = af_scenario::contract::digest(format!("{body}{extra}").as_bytes());
            let previous = source["revision"].as_str().unwrap();
            source["text"] = (issue_text.replace(&previous[7..], &revision[7..]) + extra).into();
            source["revision"] = revision.into();
        }
        save(&input, &supplied);

        let provider_home = root.join("provider");
        let bin = root.join("bin");
        fs::create_dir(&provider_home).unwrap();
        fs::create_dir(&bin).unwrap();
        let codex = bin.join("codex");
        fs::write(
            &codex,
            crate::provider_cli::with_help("codex", &crate::provider_cli::with_probe(FAKE_CODEX)),
        )
        .unwrap();
        fs::set_permissions(codex, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("providers.toml"),
            toml::to_string(&json!({"version":4,"providers":[
                {"id":format!("portable-{issue}"),"kind":"codex","auth_dir":provider_home}
            ]}))
            .unwrap(),
        )
        .unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            tasks: root.join("tasks"),
            scenarios: root.join("scenarios"),
            root,
            repo,
            input,
            path,
            provider_home,
        }
    }

    fn af(&self, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(&self.repo)
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.root.join("providers.toml"))
            .args(args)
            .output()
            .unwrap()
    }

    fn prepare(&self, bindings: Option<&Path>) -> Output {
        let mut args = vec![
            "scenario",
            "prepare",
            "--file",
            ARTIFACT,
            "--task-state",
            self.tasks.to_str().unwrap(),
            "--inputs",
            self.input.to_str().unwrap(),
            "--json",
        ];
        if let Some(bindings) = bindings {
            args.extend(["--bindings", bindings.to_str().unwrap()]);
        }
        self.af(&args)
    }
}

fn ok(output: Output) -> Value {
    assert!(output.status.success(), "{}", text(&output));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn the_same_portable_triage_runs_in_two_clean_receivers_with_typed_issue_sources() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    let mut identities = Vec::new();
    let triage_schema = schemas::validator("issue-triage-v1.json");

    let fixture = workspace().join(STAGED);
    let artifact = read(&fixture.join("scenario.json"));
    schemas::valid(&schemas::validator("scenario-artifact-v1.json"), &artifact);
    let schema_dependency = &artifact["schemas"]["issue-triage"]["path"];
    let mut portable_schema = read(&fixture.join(schema_dependency.as_str().unwrap()));
    let canonical_schema = read(&workspace().join("schemas/issue-triage-v1.json"));
    portable_schema["$id"] = canonical_schema["$id"].clone();
    assert_eq!(portable_schema, canonical_schema);

    for issue in [101, 202] {
        let receiver = Receiver::new(parent.join(issue.to_string()), issue);
        let bindings = receiver.root.join("bindings.json");
        save(
            &bindings,
            &json!({"schema": "af.task-bindings/1", "providers": {
                "backlog/triage-verifier": format!("portable-{issue}")
            }}),
        );
        let bundle = ok(receiver.prepare(Some(&bindings)));
        schemas::valid(&schemas::validator("scenario-bundle-v10.json"), &bundle);
        let bundle_path = receiver.root.join("prepared.json");
        save(&bundle_path, &bundle);
        let root = &bundle["definitions"][bundle["root"].as_str().unwrap()];
        let template_id = root["authority"]["task_templates"][0].as_str().unwrap();
        let policy_path = receiver.root.join("policy.json");
        let mut policy = json!({
            "schema": "af/scenario-policy@8", "task_plans": [], "task_templates": [],
            "adapters": {}, "decision_keys": {}, "file_roots": []
        });
        save(&policy_path, &policy);
        fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
        let paths = [
            "--file",
            bundle_path.to_str().unwrap(),
            "--authority",
            policy_path.to_str().unwrap(),
            "--state",
            receiver.scenarios.to_str().unwrap(),
            "--json",
        ];
        let validate = [&["scenario", "validate"][..], &paths].concat();
        let denied = receiver.af(&validate);
        assert!(!denied.status.success(), "{}", text(&denied));
        assert!(text(&denied).contains("template"), "{}", text(&denied));
        policy["task_templates"] = json!([template_id]);
        save(&policy_path, &policy);
        let pin = ok(receiver.af(&validate));
        let pin_id = pin["revision"].as_str().unwrap();

        let empty = receiver.root.join("empty.json");
        fs::write(&empty, "{}").unwrap();
        let start = [
            &[
                "scenario",
                "start",
                "one",
                "--inputs",
                empty.to_str().unwrap(),
                "--execute",
            ][..],
            &paths,
        ]
        .concat();
        let denied = receiver.af(&[&start[..], &["--confirm-pin", template_id]].concat());
        assert!(
            text(&denied).contains("confirmation differs"),
            "{}",
            text(&denied)
        );
        assert!(!denied.status.success());
        assert!(!receiver.provider_home.join("calls").exists());
        // Execution restores the captured sources and Provider alias.
        fs::remove_file(&receiver.input).unwrap();
        fs::remove_file(&bindings).unwrap();
        let run = ok(receiver.af(&[&start[..], &["--confirm-pin", pin_id]].concat()));
        assert_eq!(run["status"], "succeeded", "{}", run["failure"]);
        let receipt = &run["frames"]["root"]["outputs"]["triage"]["value"];
        let task_id = receipt["task_id"].as_str().unwrap();
        assert_eq!(receipt["result"]["acceptance"], "satisfied");
        let report = receiver.af(&[
            "task",
            "output",
            task_id,
            "--port",
            "report",
            "--format",
            "canonical-markdown",
            "--state",
            receiver.tasks.to_str().unwrap(),
        ]);
        assert!(report.status.success(), "{}", text(&report));
        assert!(
            text(&report).contains(&format!("Issue #{issue}")),
            "{}",
            text(&report)
        );
        let cas = Cas::open_existing(receiver.tasks.join("cas")).unwrap();
        let report = cas
            .get_artifact(
                receipt["result"]["outputs"]["report"]["artifact_ids"][0]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let draft = cas
            .get_artifact(report.payload["draft_id"].as_str().unwrap())
            .unwrap();
        let block = draft.payload["sections"][0]["body"].as_str().unwrap();
        let triage: Value = serde_json::from_str(
            block
                .strip_prefix("```json\n")
                .unwrap()
                .strip_suffix("\n```")
                .unwrap(),
        )
        .unwrap();
        schemas::valid(&triage_schema, &triage);
        assert_eq!(triage["issue"], issue);

        let authority = cas
            .get_json(
                root["steps"][0]["operation"]["template"]["authority_id"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        for worker in ["backlog/triage-author", "backlog/triage-verifier"] {
            assert_eq!(authority["providers"][worker], format!("portable-{issue}"));
        }
        assert!(authority["report_policy_id"].is_string());
        let calls = fs::read_to_string(receiver.provider_home.join("calls")).unwrap();
        assert_eq!(
            calls.lines().filter(|stage| *stage == "admission").count(),
            2
        );
        assert_eq!(
            calls
                .lines()
                .filter(|stage| *stage != "admission")
                .collect::<Vec<_>>(),
            ["author", "verifier"]
        );
        identities.push((
            template_id.to_owned(),
            pin_id.to_owned(),
            task_id.to_owned(),
        ));
    }
    assert_ne!(identities[0].0, identities[1].0);
    assert_ne!(identities[0].1, identities[1].1);
    assert_ne!(identities[0].2, identities[1].2);
}

#[test]
fn portable_prepare_refuses_missing_tampered_and_escaping_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::new(
        directory.path().canonicalize().unwrap().join("receiver"),
        101,
    );
    let artifact_path = receiver.repo.join(ARTIFACT);
    let artifact = read(&artifact_path);
    let schema_path = receiver
        .repo
        .join(STAGED)
        .join("portable-issue-triage-v1.json");
    let original = fs::read(&schema_path).unwrap();
    let assert_denied = |message: &str| {
        let denied = receiver.prepare(None);
        assert!(!denied.status.success(), "{}", text(&denied));
        assert!(text(&denied).contains(message), "{}", text(&denied));
    };
    fs::remove_file(&schema_path).unwrap();
    capture(&receiver.repo);
    assert_denied("Snapshot lacks");
    fs::write(&schema_path, "{}").unwrap();
    capture(&receiver.repo);
    assert_denied("digest differs");
    fs::remove_file(&schema_path).unwrap();
    symlink("../../CONTRIBUTING.md", &schema_path).unwrap();
    capture(&receiver.repo);
    assert_denied("symlink");
    fs::remove_file(&schema_path).unwrap();
    fs::write(&schema_path, original).unwrap();

    let worker = receiver
        .repo
        .join(STAGED)
        .join("task-packages/backlog/triage-author/instructions.md");
    let original = fs::read(&worker).unwrap();
    fs::remove_file(&worker).unwrap();
    capture(&receiver.repo);
    assert_denied("changed since it was locked");
    fs::write(&worker, "Tampered").unwrap();
    capture(&receiver.repo);
    assert_denied("changed since it was locked");
    fs::write(&worker, original).unwrap();

    let mut escaped = artifact.clone();
    escaped["packages"]["backlog/triage-author"]["path"] = "../../elsewhere".into();
    save(&artifact_path, &escaped);
    capture(&receiver.repo);
    assert_denied("beneath the artifact directory");

    save(&artifact_path, &artifact);
    capture(&receiver.repo);
    save(
        &receiver.input,
        &json!({"schema":"af.document-sources/1","sources":{}}),
    );
    assert_denied("exact declared named report sources");
    assert!(!receiver.provider_home.join("calls").exists());
}

#[test]
fn portable_prepare_rejects_package_bindings_before_inspecting_package_paths() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::new(
        directory.path().canonicalize().unwrap().join("receiver"),
        101,
    );
    let bindings = receiver.root.join("bindings.json");
    let assert_aliases_only = || {
        let denied = receiver.prepare(Some(&bindings));
        assert!(!denied.status.success(), "{}", text(&denied));
        assert!(
            text(&denied).contains("Portable Scenario bindings can only supply Provider aliases"),
            "{}",
            text(&denied)
        );
    };
    save(
        &bindings,
        &json!({"schema": "af.task-bindings/1", "packages": {"local/worker": {
            "version": "1.0.0", "digest": af_scenario::contract::digest(b"fixture"),
            "path": "local-worker"
        }}}),
    );
    // A package path lookup would fail on the missing directory.
    assert_aliases_only();
    let package = receiver.root.join("local-worker");
    fs::create_dir(&package).unwrap();
    symlink("missing", package.join("worker.toml")).unwrap();
    // A recursive capture would fail on the nested symlink, before the policy error.
    assert_aliases_only();

    save(
        &bindings,
        &json!({"schema": "af.task-bindings/1", "slots": {"author": "local/worker"}}),
    );
    assert_aliases_only();
    assert!(!receiver.provider_home.join("calls").exists());
}
