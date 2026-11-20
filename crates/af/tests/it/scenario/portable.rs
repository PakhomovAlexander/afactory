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
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const ARTIFACT: &str = "fixtures/issue-triage/scenario.json";
const STAGED: &str = "fixtures/issue-triage";
const BACKLOG: &str = "fixtures/project-backlog/scenario.json";
const FAKE_CODEX: &str = r#"#!/usr/bin/python3 -B
import hashlib, json, os, re, subprocess, sys

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
        reconsidering = batch['reference']['high_water'] == '7'
        assert batch['reference']['high_water'] in ('6', '7')
        assert len(batch['members']) == (2 if reconsidering else 6)
        assert 'AF_BACKLOG_GITHUB_TOKEN' not in os.environ
        assert 'private-attachment' not in request
        assert 'mismatched-attachment' not in request
        ids_by_evidence = {}
        for member in batch['members']:
            for evidence in member['evidence_ids']:
                ids_by_evidence.setdefault(evidence, []).append(member['member_id'])
        def identity(data):
            return 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\x00'+data).hexdigest()
        def decision(members, evidence, disposition, claim, citation=None, proposed=None):
            return dict(members=members, evidence_ids=evidence,
                        citations=[citation] if citation else [], disposition=disposition,
                        claim=claim, reason='Checked the current source behavior.',
                        tracked_id=None, proposed=proposed)
        decisions = []
        citations = []
        groups = [('blue', 3, ['blue-a', 'blue-b']), ('green', 7, ['green'])]
        if reconsidering:
            member = next(member for member in batch['members'] if member['report_id'] == 'blue-a')
            assert member['item_id']
            groups = [('blue', 3, [member['member_id']])]
        for color, boundary, members in groups:
            path = color+'-boundary.py'
            # Observe the exact committed program. The boundary is excluded and the next value is accepted.
            assert subprocess.check_output([sys.executable, path, str(boundary)]).strip() == b'False'
            assert subprocess.check_output([sys.executable, path, str(boundary+1)]).strip() == b'True'
            with open(path, 'rb') as script:
                evidence = identity(script.read())
            assert ids_by_evidence[evidence] == members
            citation = {'path':path, 'line':2}
            citations.append(citation)
            claim = color.title()+' excludes its required boundary.'
            public_body = ('The boundary should be accepted alongside higher values.' if reconsidering
                           else 'The configured boundary must be inclusive.')
            decisions.append(decision(members, [evidence], 'verified', claim, citation,
                                      {'title':claim, 'body':public_body}))
        if reconsidering:
            decisions.append(decision(['late'], [], 'needs_evidence', 'The reported behavior is unknown.'))
        else:
            decisions.append(decision(['unknown'], [], 'needs_evidence', 'The reported behavior is unknown.'))
            decisions.append(decision(['hold'], [], 'held', 'The evidence is unavailable for release.'))
            # Exercise the kernel's digest guard even if the mock independent verifier passes this group.
            mismatch = identity(b'mismatched-attachment')
            decisions.append(decision(['mismatch'], [mismatch], 'verified', 'An unrelated claim.',
                                      citations[0], {'title':'Unrelated claim', 'body':'Do not publish.'}))
        assessment = dict(schema='af/backlog-assessment@1', batch_id=batch['reference']['batch_id'],
                          source_revision=batch['source_revision'], decisions=decisions)
        body = '```json\n' + json.dumps(assessment) + '\n```'
        block = ('Blue was reconsidered. Needs evidence: late.' if reconsidering else
                 'Blue: blue-a, blue-b. Green: green. Needs evidence: unknown. Held: hold.')
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
                    *({'kind':'repository','location':c} for c in citations)]}]}
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
            for (color, boundary) in [("blue", 3), ("green", 7)] {
                fs::write(
                    repo.join(format!("{color}-boundary.py")),
                    format!("import sys\nprint(int(sys.argv[1]) > {boundary})\n"),
                )
                .unwrap();
            }
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
        self.command(repo, args).output().unwrap()
    }

    fn command(&self, repo: &Path, args: &[&str]) -> Command {
        let mut command = crate::common::af();
        command
            .current_dir(repo)
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.root.join("providers.toml"))
            .args(args);
        command
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
        schemas::valid(&schemas::validator("scenario-bundle-v11.json"), &bundle);
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

const GITHUB_ADAPTER: &str = "backlog-github-adapter";
const GITHUB_CONFIG: &str = "backlog-github-destination-v1.json";

fn private_write(path: &Path, bytes: impl AsRef<[u8]>) {
    use std::io::Write as _;
    af_store::local_fs::private_file_options()
        .create_new(true)
        .write(true)
        .open(path)
        .unwrap()
        .write_all(bytes.as_ref())
        .unwrap();
}

fn github_fixture(receiver: &Receiver) -> String {
    let state = receiver.root.join("fake-github");
    fs::create_dir(&state).unwrap();
    include_str!("../../../../../fixtures/project-backlog/fake-github-adapter.py").replace(
        "__AF_BACKLOG_FIXTURE_STATE__",
        &serde_json::to_string(&state).unwrap(),
    )
}

fn capture_backlog_batch(receiver: &Receiver, github: Option<&str>) -> Value {
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
    let blue = fs::read_to_string(receiver.repo.join("blue-boundary.py")).unwrap();
    let green = fs::read_to_string(receiver.repo.join("green-boundary.py")).unwrap();
    record(&receiver.repo, "blue-a", Some(&blue));
    record(&producer_repo, "blue-b", Some(&blue));
    record(&receiver.repo, "green", Some(&green));
    record(&producer_repo, "hold", Some("private-attachment"));
    record(&receiver.repo, "unknown", None);
    record(&receiver.repo, "mismatch", Some("mismatched-attachment"));
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
    let definition: Value = serde_json::from_slice(&artifact).unwrap();
    let dispatches = &definition["definition"]["limits"]["max_dispatches"];
    let destination = if let Some(executable) = github {
        let config = json!({
            "schema": "af/backlog-github-destination@1",
            "project_id": project_id,
            "repository": "fixture/backlog",
            "repository_id": 23,
            "publisher_id": 42,
            "executable_digest": af_store::canonical::blob_content_id(executable.as_bytes()),
        });
        private_write(
            &store.join(GITHUB_CONFIG),
            serde_json::to_vec(&config).unwrap(),
        );
        private_write(&store.join("backlog-github-token"), "fixture_token\n");
        af_store::canonical::content_id(&config).unwrap()
    } else {
        af_project::backlog::batch::local_destination(project_id).unwrap()
    };
    save(
        &store.join("backlog-batch-policy-v1.json"),
        &json!({
            "schema":"af/backlog-batch-policy@1", "project_id":project_id,
            "authority_id":af_store::canonical::blob_content_id(&authority.pk.to_bytes()),
            "scenario_id":af_scenario::contract::digest(&artifact),
            "destination_id":destination,
            "timezone":"UTC", "hour":0, "minute":0,
            "start_unix_ms":1772755200000_u64, "lateness_ms":60000,
            "run_tokens":150000, "run_wall_ms":1230000, "run_dispatches":dispatches,
            "rolling_tokens":150000, "rolling_window_ms":86400000,
            "publication_limit":1, "evidence_access":"evidence_ids", "hold_policy":"allow_local_reservations"
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
    for id in ["blue-a", "blue-b", "green", "unknown", "hold", "mismatch"] {
        accept(id);
    }
    let audit = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", "1772755200000"]));
    assert_eq!(audit["high_water"], "6");
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
fn portable_backlog_settles_and_dispatches_the_frozen_batch_in_two_receivers() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap();
    let mut receipts = Vec::new();
    schemas::valid(
        &schemas::validator("backlog-scenario-artifact-v1.json"),
        &read(&workspace().join(BACKLOG)),
    );
    for issue in [303, 404] {
        let receiver = Receiver::for_artifact(parent.join(issue.to_string()), issue, BACKLOG);
        let reference = capture_backlog_batch(&receiver, None);
        schemas::valid(
            &schemas::validator("backlog-batch-reference-v1.json"),
            &reference,
        );
        let mut wrong = reference.clone();
        wrong["high_water"] = json!("7");
        save(&receiver.input, &wrong);
        let refused = receiver.prepare_artifact(BACKLOG, None);
        assert!(!refused.status.success() && text(&refused).contains("exact cutoff"));
        assert!(!receiver.provider_home.join("calls").exists());

        save(&receiver.input, &reference);
        let bundle = ok(receiver.prepare_artifact(BACKLOG, None));
        schemas::valid(&schemas::validator("scenario-bundle-v11.json"), &bundle);
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
        assert_eq!(collected["members"].as_array().unwrap().len(), 6);
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
        let project = ok(receiver.af(&["project", "show", "--json"]));
        let store_path = Path::new(project["store"].as_str().unwrap());
        let sink = store_path.join("backlog-local-sink");
        if issue == 303 {
            // Stop after the accepted Task receipt and before any Project mutation. The
            // shared observation lock blocks the native settlement writer at that boundary.
            let guard = af_project::ProjectStore::open_read_only(
                &receiver.root.join("state"),
                reference["project_id"].as_str().unwrap(),
            )
            .unwrap();
            let mut child = receiver
                .command(&receiver.repo, &resume)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let started = Instant::now();
            let mut settlement_started = false;
            while started.elapsed() < Duration::from_secs(60) && child.try_wait().unwrap().is_none()
            {
                let observed = ok(receiver.af(&[
                    "scenario",
                    "show",
                    "daily",
                    "--state",
                    receiver.scenarios.to_str().unwrap(),
                    "--json",
                ]));
                let steps = observed["frames"]["root"]["steps"].as_array().unwrap();
                if steps.len() == 2 && steps[1]["intents"][0]["phase"] == "prepared" {
                    assert_eq!(steps[0]["status"], "succeeded");
                    settlement_started = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            child.wait().unwrap();
            assert!(
                settlement_started,
                "Native settlement never reached its durable intent"
            );
            assert!(guard.verified_backlog().unwrap().is_empty());
        }

        // The obstruction stops delivery after the native assessment, atomic settlement and
        // durable publication intent. Resume must reconcile that exact intent.
        fs::write(&sink, "temporary sink obstruction").unwrap();
        let interrupted = ok(receiver.af(&resume));
        let run_schema = schemas::validator("scenario-run-v12.json");
        schemas::valid(&run_schema, &interrupted);
        assert_eq!(
            interrupted["status"], "waiting",
            "{}",
            interrupted["failure"]
        );
        let trace = interrupted["frames"]["root"]["steps"].as_array().unwrap();
        assert_eq!(trace[0]["step"], "analyze-and-verify");
        assert_eq!(trace[1]["step"], "settle-backlog");
        assert_eq!(trace[1]["status"], "succeeded");
        assert_eq!(trace[2]["step"], "dispatch-backlog");
        assert_eq!(trace[2]["status"], "waiting");

        let event_store =
            af_store::EventStore::open_read_only(store_path.join("events.sqlite")).unwrap();
        let processing = || event_store.replay("backlog-processing").unwrap();
        let publications = || {
            processing()
                .into_iter()
                .filter_map(|event| {
                    let record: af_contracts::backlog::BacklogRecordV1 =
                        serde_json::from_value(event.payload).unwrap();
                    match record {
                        af_contracts::backlog::BacklogRecordV1::Publication(value) => Some(value),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        };
        let intents = publications();
        assert_eq!(intents.len(), 1);
        assert!(intents[0].receipt_id.is_none());
        assert_eq!(intents[0].attempt_ids.len(), 1);

        let visible = ok(receiver.af(&["backlog", "list", "--json"]));
        let items = visible["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let blue = items
            .iter()
            .find(|item| item["public"]["title"] == "Blue excludes its required boundary.")
            .unwrap();
        let green = items
            .iter()
            .find(|item| item["public"]["title"] == "Green excludes its required boundary.")
            .unwrap();
        assert_ne!(blue["item_id"], green["item_id"]);
        assert_eq!(blue["revision_id"], intents[0].revision_id);
        let private_outcome = |id| {
            let data = ok(receiver.af(&["backlog", "show", id, "--private", "--json"]));
            let outcomes = data["data"]["outcomes"].as_array().unwrap();
            assert_eq!(outcomes.len(), 1);
            outcomes[0].clone()
        };
        assert_eq!(private_outcome("blue-a"), private_outcome("blue-b"));
        assert_eq!(private_outcome("green")["disposition"], "verified");
        assert_eq!(private_outcome("hold")["disposition"], "held");
        assert_eq!(private_outcome("mismatch")["disposition"], "held");
        assert_eq!(private_outcome("unknown")["disposition"], "needs_evidence");
        assert!(!visible.to_string().contains("attachment"));

        fs::remove_file(&sink).unwrap();
        let entry = sink.join(format!("{}.json", &intents[0].intent_id[7..]));
        if issue == 404 {
            // Model a lost acknowledgement after the synthetic sink stored the approved bytes.
            // The next resume must recognize the same delivery and record its receipt.
            af_store::local_fs::create_private_dir_all(&sink).unwrap();
            fs::write(&entry, serde_json::to_vec(&json!({
                "item_id":blue["item_id"], "revision_id":blue["revision_id"], "public":blue["public"]
            })).unwrap()).unwrap();
        }
        let run = ok(receiver.af(&resume));
        schemas::valid(&run_schema, &run);
        let dispatch = &run["frames"]["root"]["steps"][2];
        assert_eq!(dispatch["intents"][0]["id"], trace[2]["intents"][0]["id"]);
        assert_eq!(dispatch["intents"][0]["phase"], "done");
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
        assert_eq!(read(&entry), *blue);
        assert_eq!(fs::read_dir(&sink).unwrap().count(), 1);
        let settled = publications();
        assert_eq!(settled.len(), 2);
        assert_eq!(settled[0], intents[0]);
        assert_eq!(settled[1].intent_id, intents[0].intent_id);
        assert_eq!(settled[1].attempt_ids, intents[0].attempt_ids);
        assert_eq!(
            settled[1].receipt_id.as_deref(),
            Some(af_store::canonical::blob_content_id(&fs::read(&entry).unwrap()).as_str())
        );
        let audit = ok(receiver.af(&["backlog", "batch", "audit"]));
        assert!(audit["active_batch_id"].is_null());
        assert_eq!(audit["closed_high_water"], "6");
        assert_eq!(audit["pending_member_ids"], json!(["late"]));
        assert_eq!(ok(receiver.af(&resume)), run);
        assert_eq!(publications(), settled);
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

fn start_github_backlog(receiver: &Receiver, id: &str, batch: &Value) {
    let bundle_path = receiver.root.join("prepared.json");
    let policy_path = receiver.root.join("policy.json");
    if !policy_path.exists() {
        private_write(&policy_path, "{}");
    }
    let empty = receiver.root.join("empty.json");
    fs::write(&empty, "{}").unwrap();
    let paths = [
        "--file",
        bundle_path.to_str().unwrap(),
        "--authority",
        policy_path.to_str().unwrap(),
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ];
    save(&receiver.input, batch);
    let bundle = ok(receiver.prepare_artifact(BACKLOG, None));
    let root = &bundle["definitions"]["daily-backlog"];
    save(&bundle_path, &bundle);
    save(
        &policy_path,
        &json!({
            "schema":"af/scenario-policy@8",
            "task_plans":[], "task_templates":root["authority"]["task_templates"],
            "adapters":{}, "decision_keys":{}, "file_roots":[]
        }),
    );
    let pin = ok(receiver.af(&[&["scenario", "validate"][..], &paths].concat()));
    let start = [
        &[
            "scenario",
            "start",
            id,
            "--inputs",
            empty.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
        ][..],
        &paths,
    ]
    .concat();
    ok(receiver.af(&start));
}

fn configure_github_fixture_bounds(receiver: &Receiver) {
    // Give this test-only artifact one dispatch per scripted fault and recovery. The
    // shipped artifact and synthetic-sink tests keep their ordinary five-dispatch limit.
    let artifact = receiver.repo.join(BACKLOG);
    let mut definition = read(&artifact);
    definition["definition"]["limits"]["max_dispatches"] = json!(11);
    save(&artifact, &definition);
    capture(&receiver.repo);
}

#[test]
fn portable_backlog_github_reconciles_a_lost_create_without_duplicate_writes() {
    use af_contracts::backlog::{BacklogRecordV1, Publication};

    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        505,
        BACKLOG,
    );
    configure_github_fixture_bounds(&receiver);
    let executable = github_fixture(&receiver);
    let reference = capture_backlog_batch(&receiver, Some(&executable));
    start_github_backlog(&receiver, "github-backlog", &reference);
    let resume = [
        "scenario",
        "resume",
        "github-backlog",
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ];
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store_path = Path::new(project["store"].as_str().unwrap());
    let database = af_store::EventStore::open_read_only(store_path.join("events.sqlite")).unwrap();
    let cas = Cas::open_existing(store_path.join("cas")).unwrap();
    let publications = || {
        database
            .replay("backlog-processing")
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                match serde_json::from_value::<BacklogRecordV1>(event.payload).unwrap() {
                    BacklogRecordV1::Publication(value) => {
                        assert_eq!(event.artifact_refs, value.provider_state_id.as_slice());
                        Some(value)
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
    };
    let state_schema = schemas::validator("backlog-github-state-v1.json");
    let state = |publication: &Publication| {
        let state = cas
            .get_json(publication.provider_state_id.as_deref().unwrap())
            .unwrap();
        schemas::valid(&state_schema, &state);
        state
    };
    #[track_caller]
    fn check_pending(output: Output) {
        let run = ok(output);
        assert_eq!(run["status"], "waiting", "{}", run["failure"]);
    }
    let pending = || receiver.af(&resume);
    let fake = receiver.root.join("fake-github");

    // The native assessment and intent survive an interruption before any executable can send.
    check_pending(pending());
    let intents = publications();
    assert_eq!(intents.len(), 2);
    assert_eq!(state(&intents[1])["outcome"]["kind"], "adapter_unavailable");
    assert!(intents[0].attempt_ids.is_empty());
    assert!(intents[0].receipt_id.is_none());
    assert!(!fake.join("requests").exists());
    let visible = ok(receiver.af(&["backlog", "list", "--json"]));
    assert_eq!(visible["data"]["items"].as_array().unwrap().len(), 2);
    assert!(!visible.to_string().contains("private-attachment"));
    assert!(!store_path.join("backlog-local-sink").exists());

    // A changed executable cannot launch. The pinned executable then creates an issue and
    // exits without a response.
    let adapter = store_path.join(GITHUB_ADAPTER);
    private_write(&adapter, "unpinned bytes");
    check_pending(pending());
    assert!(!fake.join("requests").exists());
    fs::remove_file(&adapter).unwrap();
    private_write(&adapter, &executable);
    check_pending(pending());
    assert_eq!(
        state(publications().last().unwrap())["outcome"]["kind"],
        "transport_unavailable"
    );
    let issue = read(&fake.join("issue.json"));

    // Receiver authority can be removed while an intent is waiting.
    let config = store_path.join(GITHUB_CONFIG);
    let saved = fs::read(&config).unwrap();
    fs::remove_file(&config).unwrap();
    let before = publications();
    check_pending(pending());
    let revoked = publications();
    assert_eq!(revoked.len(), before.len() + 1);
    assert_eq!(
        state(revoked.last().unwrap())["outcome"]["kind"],
        "authority_unavailable"
    );
    assert_eq!(
        revoked.last().unwrap().attempt_ids,
        before.last().unwrap().attempt_ids
    );
    private_write(&config, saved);

    for (mode, expected) in [
        ("delayed", "not_observed"),
        ("conflict", "identity_conflict"),
        ("wrong_marker", "identity_conflict"),
        ("wrong_repository", "identity_conflict"),
    ] {
        fs::write(fake.join("mode"), mode).unwrap();
        check_pending(pending());
        let history = publications();
        assert_eq!(state(history.last().unwrap())["outcome"]["kind"], expected);
        assert!(history.last().unwrap().receipt_id.is_none());
    }

    let mut human = issue;
    human["state"] = json!("closed");
    human["fields_id"] = json!(af_store::canonical::blob_content_id(b"human edit"));
    save(&fake.join("issue.json"), &human);
    fs::write(fake.join("mode"), "normal").unwrap();
    let mut concurrent = receiver
        .command(&receiver.repo, &resume)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = receiver.af(&resume);
    concurrent.wait().unwrap();
    assert_eq!(ok(receiver.af(&resume))["status"], "succeeded");

    let history = publications();
    let final_record = history.last().unwrap();
    assert_eq!(final_record.intent_id, intents[0].intent_id);
    assert_eq!(final_record.remote_id.as_deref(), Some("444:1"));
    assert!(final_record.receipt_id.is_some());
    assert_eq!(final_record.attempt_ids.len(), 6);
    let observed = state(final_record);
    assert_eq!(observed["outcome"]["state"], "closed");
    assert_eq!(observed["outcome"]["approved_fields_match"], false);
    assert_eq!(read(&fake.join("issue.json")), human);
    let requests = fs::read_to_string(fake.join("requests")).unwrap();
    let requests: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 6);
    assert_eq!(requests[0]["operation"], "create");
    assert!(
        requests[1..]
            .iter()
            .all(|request| request["operation"] == "reconcile")
    );
    assert!(
        requests
            .iter()
            .all(|request| request["item_id"] == intents[0].item_id
                && request["revision_id"] == intents[0].revision_id)
    );

    // A later independently verified revision reconciles the same immutable issue and preserves
    // the operator's edit and closure. Move to a new source so the deferred green item needs proof.
    let audit = ok(receiver.af(&["backlog", "batch", "audit"]));
    let closed = audit["batches"][reference["batch_id"].as_str().unwrap()]["closed_unix_ms"]
        .as_u64()
        .unwrap();
    let due = ((closed / 86_400_000 + 2) * 86_400_000).to_string();
    ok(receiver.af(&[
        "backlog",
        "batch",
        "reconsider",
        "--item-id",
        &final_record.item_id,
        "--report-id",
        "blue-a",
        "--eligible-unix-ms",
        &due,
    ]));
    fs::write(
        receiver.repo.join("CONTRIBUTING.md"),
        "# New source revision\n",
    )
    .unwrap();
    capture(&receiver.repo);
    let audit = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", &due]));
    let revised_reference = json!({
        "schema":"af/backlog-batch-reference@1",
        "project_id":reference["project_id"],
        "batch_id":audit["active_batch_id"],
        "policy_id":audit["pin_id"],
        "high_water":audit["high_water"],
    });
    start_github_backlog(&receiver, "revised-github-backlog", &revised_reference);
    let mut revised_resume = resume;
    revised_resume[2] = "revised-github-backlog";
    assert_eq!(ok(receiver.af(&revised_resume))["status"], "succeeded");
    let revised_history = publications();
    let revised = revised_history.last().unwrap();
    assert_eq!(revised.item_id, final_record.item_id);
    assert_ne!(revised.revision_id, final_record.revision_id);
    assert_eq!(revised.remote_id, final_record.remote_id);
    assert!(revised.receipt_id.is_some());
    assert_ne!(revised.receipt_id, final_record.receipt_id);
    let requests = fs::read_to_string(fake.join("requests")).unwrap();
    assert_eq!(requests.lines().count(), 7);
    let request: Value = serde_json::from_str(requests.lines().last().unwrap()).unwrap();
    assert_eq!(request["operation"], "reconcile");
    assert_eq!(request["issue"], json!({"id":444, "number":1}));
    assert_eq!(request["revision_id"], revised.revision_id);
    assert_eq!(
        request["marker"],
        serde_json::from_str::<Value>(requests.lines().next().unwrap()).unwrap()["marker"]
    );
    assert_eq!(read(&fake.join("issue.json")), human);
    let visible = ok(receiver.af(&["backlog", "show", &revised.item_id, "--json"]));
    assert_eq!(visible["data"]["revision_id"], revised.revision_id);
    assert_eq!(
        visible["data"]["public"]["body"],
        "The boundary should be accepted alongside higher values."
    );
}

#[test]
fn portable_backlog_github_retains_a_crash_before_the_provider_write_as_pending() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        506,
        BACKLOG,
    );
    let executable = github_fixture(&receiver);
    let reference = capture_backlog_batch(&receiver, Some(&executable));
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store_path = Path::new(project["store"].as_str().unwrap());
    private_write(&store_path.join(GITHUB_ADAPTER), &executable);
    let fake = receiver.root.join("fake-github");
    fs::write(fake.join("abort-before-write"), "").unwrap();
    start_github_backlog(&receiver, "before-write", &reference);
    let resume = [
        "scenario",
        "resume",
        "before-write",
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ];
    let first = ok(receiver.af(&resume));
    assert_eq!(first["status"], "waiting", "{}", first["failure"]);
    assert!(!fake.join("issue.json").exists());
    fs::remove_file(fake.join("abort-before-write")).unwrap();
    fs::write(fake.join("mode"), "delayed").unwrap();
    let second = ok(receiver.af(&resume));
    assert_eq!(second["status"], "waiting", "{}", second["failure"]);
    assert!(!fake.join("issue.json").exists());

    let requests = fs::read_to_string(fake.join("requests")).unwrap();
    let trace: Vec<Value> = requests
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(trace.len(), 2);
    assert_eq!(trace[0]["operation"], "create");
    assert_eq!(trace[1]["operation"], "reconcile");
    let database = af_store::EventStore::open_read_only(store_path.join("events.sqlite")).unwrap();
    let events = database.replay("backlog-processing").unwrap();
    let publication = &events.last().unwrap().payload;
    assert_eq!(
        publication["attempt_ids"],
        json!([trace[0]["request_id"], trace[1]["request_id"]])
    );
    assert!(publication["remote_id"].is_null());
    assert!(publication["receipt_id"].is_null());
}
