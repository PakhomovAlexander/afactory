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
const BACKLOG: &str = "fixtures/project-backlog/scenario.json";
const FAKE_CODEX: &str = r#"#!/usr/bin/python3 -B
import hashlib, json, os, re, sys

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
    if 'backlog-batch' in sources:
        batch = json.loads(sources['backlog-batch']['text'])
        assert batch['reference']['high_water'] == '5'
        assert len(batch['members']) == 5
        assert 'private-attachment' not in request
        ids_by_evidence = {}
        for member in batch['members']:
            for evidence in member['evidence_ids']:
                ids_by_evidence.setdefault(evidence, []).append(member['report_id'])
        def ids(text):
            return ids_by_evidence['sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\x00'+text.encode()).hexdigest()]
        assert ids('blue boundary') == ['blue-a', 'blue-b']
        assert ids('green boundary') == ['green']
        assert ids('private-attachment') == ['hold']
        with open('boundary-rules.json', encoding='utf-8') as source_file:
            rules = json.load(source_file)
        citation = {'path':'boundary-rules.json', 'line':1}
        def decision(members, evidence, disposition, claim, proposed=None):
            return dict(members=members, evidence_ids=evidence,
                        citations=[citation] if evidence else [], disposition=disposition,
                        claim=claim, reason='Checked the current source rules.',
                        tracked_id=None, proposed=proposed)
        decisions = []
        for color in ['blue', 'green']:
            # These public source rules exclude the inclusive boundary required by the fixture.
            assert rules[color]['expected_inclusive']
            assert not rules[color]['implemented_inclusive']
            text = color+' boundary'
            evidence = 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\x00'+text.encode()).hexdigest()
            claim = color.title()+' excludes its required boundary.'
            decisions.append(decision(ids(text), [evidence], 'verified', claim,
                                      {'title':claim, 'body':'The configured boundary must be inclusive.'}))
        decisions.append(decision(['unknown'], [], 'needs_evidence', 'The reported behavior is unknown.'))
        decisions.append(decision(['hold'], [], 'held', 'The evidence is unavailable for release.'))
        assessment = dict(schema='af/backlog-assessment@1', batch_id=batch['reference']['batch_id'],
                          source_revision=batch['source_revision'], decisions=decisions)
        body = '```json\n' + json.dumps(assessment) + '\n```'
        block = 'Blue: blue-a, blue-b. Green: green. Needs evidence: unknown. Held: hold.'
        issue = sources['backlog-batch']
        if 'document' in inputs:
            stage = 'verifier'
            assert inputs['checks']['payload']['outcome'] == 'passed'
            assert block in inputs['document']['payload']['text']
            assert inputs['checks']['payload']['checks']['backlog_assessment'] == 'passed'
            outputs = {'result':[{'document_id':inputs['document']['artifact_id'],
                'sources_id':inputs['sources']['artifact_id'],
                'requirements_id':inputs['requirements']['artifact_id'],
                'check_receipt_id':inputs['checks']['artifact_id'],
                'source_snapshot_id':inputs['source']['snapshot_id'],
                'outcome':'passed','summary':'Independent assessment of frozen membership.'}]}
        else:
            stage = 'author'
            outputs = {'draft':[{'schema':'af.document-draft/3','title':'Backlog assessment',
                'sections':[{'heading':'Findings','body':block},
                    {'heading':'Recommendation','body':'Keep the assessment private.'},
                    {'heading':'Decisions','body':body}],
                'citations':[{'kind':'source','name':'backlog-batch'},
                    {'kind':'repository','location':citation}]}]}
    else:
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
        Self::for_artifact(root, issue, ARTIFACT)
    }

    fn for_artifact(root: PathBuf, issue: u64, artifact: &str) -> Self {
        fs::create_dir(&root).unwrap();
        let repo = root.join("arbitrary checkout");
        fs::create_dir_all(repo.join(".af")).unwrap();
        let directory = Path::new(artifact).parent().unwrap();
        copy_tree(&workspace().join(directory), &repo.join(directory));
        fs::write(repo.join("CONTRIBUTING.md"), "# Contribution evidence\n").unwrap();
        if artifact == BACKLOG {
            save(
                &repo.join("boundary-rules.json"),
                &json!({
                    "blue":{"expected_inclusive":true, "implemented_inclusive":false},
                    "green":{"expected_inclusive":true, "implemented_inclusive":false}
                }),
            );
        }
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
"backlog/daily-author" = "portable-{issue}"
"backlog/daily-verifier" = "portable-{issue}"
"#,
            ),
        )
        .unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        capture(&repo);

        let input = root.join("issue.json");
        if artifact == ARTIFACT {
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
                source["text"] =
                    (issue_text.replace(&previous[7..], &revision[7..]) + extra).into();
                source["revision"] = revision.into();
            }
            save(&input, &supplied);
        }

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
        self.af_in(&self.repo, args)
    }

    fn af_in(&self, repo: &Path, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(repo)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.root.join("providers.toml"))
            .args(args)
            .output()
            .unwrap()
    }

    fn prepare(&self, bindings: Option<&Path>) -> Output {
        self.prepare_artifact(ARTIFACT, bindings)
    }

    fn prepare_artifact(&self, artifact: &str, bindings: Option<&Path>) -> Output {
        let mut args = vec![
            "scenario",
            "prepare",
            "--file",
            artifact,
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

fn capture_backlog_batch(receiver: &Receiver) -> Value {
    let producer_repo = receiver.root.join("another producer");
    git(
        &receiver.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            producer_repo.to_str().unwrap(),
            "HEAD",
        ],
    );
    let evidence = receiver.root.join("evidence.txt");
    let submission = receiver.root.join("submission.json");
    let project = |repo: &Path| ok(receiver.af_in(repo, &["project", "show", "--json"]));
    let key_pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let authority = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let write_key = |name: &str, secret: &minisign::SecretKey| {
        let file = receiver.root.join(name);
        use std::io::Write as _;
        af_store::local_fs::private_file_options()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap()
            .write_all(secret.to_box(None).unwrap().into_string().as_bytes())
            .unwrap();
        file
    };
    let producer_key = write_key("producer.key", &key_pair.sk);
    let authority_key = write_key("authority.key", &authority.sk);
    let record = |repo: &Path, id: &str, evidence_text: Option<&str>| {
        let mut args = vec![
            "backlog",
            "record",
            "--report-id",
            id,
            "--title",
            "Boundary issue",
            "--observed",
            "Different behavior at a boundary",
            "--json",
        ];
        if let Some(text) = evidence_text {
            fs::write(&evidence, text).unwrap();
            args.extend(["--evidence-file", evidence.to_str().unwrap()]);
        }
        ok(receiver.af_in(repo, &args));
    };
    record(&receiver.repo, "blue-a", Some("blue boundary"));
    record(&producer_repo, "blue-b", Some("blue boundary"));
    record(&receiver.repo, "green", Some("green boundary"));
    record(&producer_repo, "hold", Some("private-attachment"));
    record(&receiver.repo, "unknown", None);
    record(&receiver.repo, "late", None);
    let project = project(&receiver.repo);
    let store = Path::new(project["store"].as_str().unwrap());
    let project_id = project["project_id"].as_str().unwrap();
    save(
        &store.join("backlog-intake-v1.json"),
        &json!({
            "schema":"af/backlog-intake-policy@1",
            "project_id":project_id,
            "authority_key":authority.pk.to_base64(),
            "producer_key":key_pair.pk.to_base64(),
            "enrolled_keys":[key_pair.pk.to_base64()]
        }),
    );
    let artifact = fs::read(receiver.repo.join(BACKLOG)).unwrap();
    save(
        &store.join("backlog-batch-policy-v1.json"),
        &json!({
            "schema":"af/backlog-batch-policy@1", "project_id":project_id,
            "authority_id":af_store::canonical::blob_content_id(&authority.pk.to_bytes()),
            "scenario_id":af_scenario::contract::digest(&artifact),
            "destination_id":format!("sha256:{}", "a".repeat(64)),
            "timezone":"UTC", "hour":0, "minute":0,
            "start_unix_ms":1772755200000_u64, "lateness_ms":60000,
            "run_tokens":150000, "run_wall_ms":1230000, "run_dispatches":1,
            "rolling_tokens":150000, "rolling_window_ms":86400000,
            "publication_limit":1, "evidence_access":"evidence_ids", "hold_policy":"hold_all"
        }),
    );
    let accept = |id| {
        let packet = ok(receiver.af(&[
            "backlog",
            "export",
            id,
            "--key",
            producer_key.to_str().unwrap(),
        ]));
        save(&submission, &packet);
        ok(receiver.af(&[
            "backlog",
            "accept",
            submission.to_str().unwrap(),
            "--key",
            authority_key.to_str().unwrap(),
        ]));
    };
    for id in ["blue-a", "blue-b", "green", "unknown", "hold"] {
        accept(id);
    }
    let audit = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", "1772755200000"]));
    assert_eq!(audit["high_water"], "5");
    accept("late");
    json!({
        "schema":"af/backlog-batch-reference@1",
        "project_id":project_id,
        "batch_id":audit["active_batch_id"],
        "policy_id":audit["pin_id"],
        "high_water":audit["high_water"]
    })
}

#[test]
fn portable_backlog_collects_and_verifies_the_frozen_batch_in_two_receivers() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    let mut receipts = Vec::new();
    schemas::valid(
        &schemas::validator("scenario-artifact-v2.json"),
        &read(&workspace().join(BACKLOG)),
    );
    for issue in [303, 404] {
        let receiver = Receiver::for_artifact(parent.join(issue.to_string()), issue, BACKLOG);
        let reference = capture_backlog_batch(&receiver);
        schemas::valid(
            &schemas::validator("backlog-batch-reference-v1.json"),
            &reference,
        );
        let mut wrong = reference.clone();
        wrong["high_water"] = json!("6");
        save(&receiver.input, &wrong);
        let refused = receiver.prepare_artifact(BACKLOG, None);
        assert!(!refused.status.success() && text(&refused).contains("exact cutoff"));
        assert!(!receiver.provider_home.join("calls").exists());

        save(&receiver.input, &reference);
        let bundle = ok(receiver.prepare_artifact(BACKLOG, None));
        schemas::valid(&schemas::validator("scenario-bundle-v10.json"), &bundle);
        let bundle_path = receiver.root.join("prepared.json");
        save(&bundle_path, &bundle);
        let root = &bundle["definitions"]["daily-backlog"];
        let collection = &root["steps"][0]["operation"]["template"]["document"]["requirements"]["scenario_portable_sources"]
            ["sources"]["backlog-batch"];
        let collected: Value = serde_json::from_str(collection["text"].as_str().unwrap()).unwrap();
        schemas::valid(
            &schemas::validator("backlog-collected-batch-v1.json"),
            &collected,
        );
        assert_eq!(collected["reference"], reference);
        assert_eq!(collected["members"].as_array().unwrap().len(), 5);
        assert!(!collection.to_string().contains("private-attachment"));
        assert!(!collection.to_string().contains("Different behavior"));
        assert!(!collection.to_string().contains("late"));

        let policy_path = receiver.root.join("policy.json");
        save(
            &policy_path,
            &json!({
                "schema":"af/scenario-policy@8",
                "task_plans":[], "task_templates":root["authority"]["task_templates"],
                "adapters":{}, "decision_keys":{}, "file_roots":[]
            }),
        );
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
        let pin = ok(receiver.af(&[&["scenario", "validate"][..], &paths].concat()));
        let empty = receiver.root.join("empty.json");
        fs::write(&empty, "{}").unwrap();
        let start = [
            &[
                "scenario",
                "start",
                "daily",
                "--inputs",
                empty.to_str().unwrap(),
                "--confirm-pin",
                pin["revision"].as_str().unwrap(),
            ][..],
            &paths,
        ]
        .concat();
        ok(receiver.af(&start));
        assert!(!receiver.provider_home.join("calls").exists());
        fs::remove_file(&receiver.input).unwrap();
        let resume = [
            "scenario",
            "resume",
            "daily",
            "--state",
            receiver.scenarios.to_str().unwrap(),
            "--json",
        ];
        let run = ok(receiver.af(&resume));
        assert_eq!(run["status"], "succeeded", "{}", run["failure"]);
        let task = &run["frames"]["root"]["outputs"]["assessment"]["value"];
        assert_eq!(task["result"]["acceptance"], "satisfied");
        assert!(
            task["charged_tokens"]
                .as_u64()
                .is_some_and(|tokens| tokens > 0)
        );
        assert!(task["result"]["outputs"]["report"]["artifact_ids"][0].is_string());
        let verification_id = task["result"]["outputs"]["verification"]["artifact_ids"][0]
            .as_str()
            .unwrap();
        let cas = Cas::open_existing(receiver.tasks.join("cas")).unwrap();
        let receipt = cas.get_artifact(verification_id).unwrap();
        assert!(receipt.payload["evaluation_id"].is_string());
        let document = cas
            .get_artifact(receipt.payload["document_id"].as_str().unwrap())
            .unwrap();
        let draft = cas
            .get_artifact(document.payload["draft_id"].as_str().unwrap())
            .unwrap();
        let assessment = draft.payload["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|section| section["heading"] == "Decisions")
            .unwrap();
        let assessment: Value = serde_json::from_str(
            assessment["body"]
                .as_str()
                .unwrap()
                .strip_prefix("```json\n")
                .unwrap()
                .strip_suffix("\n```")
                .unwrap(),
        )
        .unwrap();
        schemas::valid(
            &schemas::validator("backlog-assessment-v1.json"),
            &assessment,
        );
        assert_eq!(
            assessment["decisions"][0]["members"],
            json!(["blue-a", "blue-b"])
        );
        assert_eq!(assessment["decisions"][1]["members"], json!(["green"]));
        for (index, outcome) in [(2, "needs_evidence"), (3, "held")] {
            assert_eq!(assessment["decisions"][index]["disposition"], outcome);
            assert!(assessment["decisions"][index]["proposed"].is_null());
        }
        assert_eq!(
            receipt.payload["document_id"],
            task["result"]["outputs"]["report"]["artifact_ids"][0]
        );
        assert_eq!(
            ok(receiver.af(&resume))["frames"]["root"]["outputs"]["assessment"]["value"],
            *task
        );
        let calls = fs::read_to_string(receiver.provider_home.join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|stage| *stage != "admission")
                .collect::<Vec<_>>(),
            ["author", "verifier"]
        );
        assert_eq!(
            ok(receiver.af(&["backlog", "list", "--json"]))["data"]["items"],
            json!([])
        );
        let evaluation = cas
            .get_artifact(receipt.payload["evaluation_id"].as_str().unwrap())
            .unwrap();
        let (
            af_contracts::Producer::Attempt {
                node_id: author,
                attempt_id: author_attempt,
                ..
            },
            af_contracts::Producer::Attempt {
                node_id: verifier,
                attempt_id: verify_attempt,
                ..
            },
        ) = (&draft.producer, &evaluation.producer)
        else {
            panic!("The assessment draft and evaluation must name their recorded Attempts");
        };
        assert_ne!(author, verifier);
        assert_ne!(author_attempt, verify_attempt);
        eprintln!(
            "receiver {issue}: Task {} result {} verification {} author {author_attempt} verifier {verify_attempt} synthetic Provider charge {} tokens",
            task["task_id"], task["result_id"], verification_id, task["charged_tokens"],
        );
        receipts.push(task["result_id"].clone());
    }
    assert_ne!(receipts[0], receipts[1]);
}
