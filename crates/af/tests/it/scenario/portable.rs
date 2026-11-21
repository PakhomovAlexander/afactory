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
import glob, hashlib, json, os, re, subprocess, sys

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
    batch = json.loads(sources['backlog-batch']['text']) if 'backlog-batch' in sources else None
    if batch and batch['members'] and batch['members'][0]['report_id'].startswith('bulk-'):
        decisions, citations = [], []
        tracked = {item['revision']['claim'].splitlines()[0]: key for key, item in batch['tracked_items'].items()}
        for member in batch['members']:
            report_id = member['report_id']
            index = int(report_id.split('-')[1])
            boundary = index if index < 64 else index - (64 if index < 66 else 2)
            path = f'bulk-{boundary:03}.py'
            namespace = {}
            with open(path, 'rb') as script:
                source = script.read()
            exec(compile(source, path, 'exec'), namespace)
            assert not namespace['accepts'](boundary) and namespace['accepts'](boundary + 1)
            evidence = 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\x00'+source).hexdigest()
            assert member['evidence_ids'] == [evidence]
            assert member['report']['observed'] == f'{path} rejects its required boundary {boundary}.'
            claim = member['report']['observed']
            citation = {'path': path, 'line': 2}
            tracked_id = tracked.get(claim)
            disposition = ('already_tracked' if tracked_id else
                           'held' if batch['tracked_items_truncated'] else 'verified')
            public = None
            if disposition == 'verified':
                # Make the first two distinct revisions too large to fit together in the tracked view.
                public = {'title':f'Inclusive comparison in {path}', 'body':('Use an inclusive comparison. ' * 280).strip()
                          if boundary in (0, 1) else 'Use an inclusive comparison.'}
            if disposition != 'held':
                citations.append(citation)
            if disposition == 'verified' and boundary in (0, 1):
                claim += '\n' + ('The boundary itself fails. ' * 299).strip()
            decisions.append(dict(members=[member['member_id']], evidence_ids=[evidence],
                                  citations=[] if disposition == 'held' else [citation],
                                  disposition=disposition, claim=claim, reason='Checked the source.',
                                  tracked_id=tracked_id, provider_match=None, proposed=public))
        if not citations:
            citations.append({'path':'CONTRIBUTING.md', 'line':1})
        assessment = dict(schema='af/backlog-assessment@2', batch_id=batch['reference']['batch_id'],
                          source_revision=batch['source_revision'], decisions=decisions)
        if 'document' in inputs:
            stage = 'verifier'
            checks = inputs['checks']['payload']
            assert checks['outcome'] == 'passed' and checks['checks']['backlog_assessment'] == 'passed'
            outputs = {'result':[{'document_id':inputs['document']['artifact_id'],
                'sources_id':inputs['sources']['artifact_id'],
                'requirements_id':inputs['requirements']['artifact_id'],
                'check_receipt_id':inputs['checks']['artifact_id'],
                'source_snapshot_id':inputs['source']['snapshot_id'],
                'outcome':'passed','summary':'Verified each boundary in the captured source.'}]}
        else:
            stage = 'author'
            outputs = {'draft':[{'schema':'af.document-draft/3','title':'Distinct backlog boundaries',
                'sections':[{'heading':'Findings','body':'Each report was checked against its source.'},
                    {'heading':'Recommendation','body':'Keep the assessment private.'},
                    {'heading':'Decisions','body':'```json\n'+json.dumps(assessment)+'\n```'}],
                'citations':[{'kind':'source','name':'backlog-batch'},
                    *({'kind':'repository','location':c} for c in citations)]}]}
    elif batch:
        count = len(batch['members'])
        member_ids = {member['member_id'] for member in batch['members']}
        provider_index = batch['provider_index']
        reconsidering = count == 2 and not provider_index
        duplicate = member_ids == {'late'} and not provider_index
        if not provider_index:
            assert count in (0, 1, 2, 6)
            assert batch['reference']['high_water'] == ('7' if reconsidering or duplicate else '6')
        assert not {'AF_BACKLOG_GITHUB_TOKEN', 'AF_BACKLOG_GITHUB_SCOPE', 'GH_TOKEN',
                    'GITHUB_TOKEN', 'SSH_AUTH_SOCK', 'AF_BACKLOG_JIRA_CREDENTIAL',
                    'AF_BACKLOG_JIRA_SCOPE'} & os.environ.keys()
        assert os.environ['GH_CONFIG_DIR'] == '/dev/null'
        provider_home = os.environ['CODEX_HOME']
        assert os.path.isdir(provider_home)
        assert not glob.glob(os.path.join(os.path.dirname(provider_home),
            'state/af/project/*/backlog-github-token'))
        assert not glob.glob(os.path.join(os.path.dirname(provider_home),
            'state/af/project/*/backlog-jira-credential'))
        # Exercise Git's actual external credential helper lookup with synthetic input.
        credentials = subprocess.run(['git', 'credential', 'fill'],
            input=b'protocol=https\nhost=fixture.invalid\n\n', capture_output=True)
        assert credentials.returncode != 0 and not credentials.stdout
        ids_by_evidence = {}
        for member in batch['members']:
            report = member['report']
            assert report['report_id'] == member['report_id']
            assert report['report_digest'] == member['report_digest']
            assert report['sequence'] == member['sequence']
            assert report['privacy'] == 'private'
            assert report['observed'] == 'Different behavior at a boundary'
            if member['report_id'] == 'unknown':
                assert report['repro'] == 'private-insufficient-repro'
            if member['report_id'] == 'hold':
                assert report['evidence'][0]['text'] == 'private-attachment'
            if member['report_id'] == 'mismatch':
                assert report['evidence'][0]['text'] == 'mismatched-attachment'
            for evidence in member['evidence_ids']:
                ids_by_evidence.setdefault(evidence, []).append(member['member_id'])
        def identity(data):
            return 'sha256:'+hashlib.sha256(b'review.kernel/content-id/v1\x00'+data).hexdigest()
        def decision(members, evidence, disposition, claim, citation=None, proposed=None):
            return dict(members=members, evidence_ids=evidence,
                        citations=[citation] if citation else [], disposition=disposition,
                        claim=claim, reason='Checked the current source behavior.',
                        tracked_id=None, provider_match=None, proposed=proposed)
        decisions = []
        citations = []
        groups = [('blue', 3, ['blue-a', 'blue-b']), ('green', 7, ['green'])] if count else []
        if duplicate:
            groups = [('green', 7, ['late'])]
        if reconsidering:
            member = next(member for member in batch['members'] if member['report_id'] == 'blue-a')
            assert member['item_id']
            groups = [('blue', 3, [member['member_id']])]
        for color, boundary, members in groups:
            if not member_ids.intersection(members):
                continue
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
        if provider_index:
            assert 'fixture_issues_read_only' not in request
            assert all('fixture_issues_read_only' not in value for value in os.environ.values())
            index = provider_index['observation']
            assert index['repository_id'] == 23 and index['observation'] == 'live-page-walk'
            exact, similar = index['issues']
            assert exact['id'] == 1073 and exact['number'] == 73
            assert exact['title'] == similar['title'] and similar['id'] == 1074
            quote = 'blue-boundary.py rejects the input 3 and accepts 4.'
            assert quote in exact['body_excerpt'].splitlines()
            assert 'rejects a string before comparing' in similar['body_excerpt']
            assert all('green-boundary.py' not in issue['body_excerpt'] for issue in index['issues'])
            assert not exact['body_truncated'] and not similar['body_truncated']
            if 'blue-a' in member_ids:
                decisions[0].update(disposition='already_tracked', proposed=None,
                                    provider_match={'issue_id': exact['id'], 'quote': quote},
                                    reason='The same input and source path exhibit the same behavior.')
        if duplicate:
            tracked = next((key, value) for key, value in batch['tracked_items'].items()
                           if value['revision']['claim'] == 'Green excludes its required boundary.')
            key, item = tracked
            assert item['revision']['item_id'] == key and item['verification_id'].startswith('sha256:')
            assert item['revision']['evidence_ids'] == decisions[0]['evidence_ids']
            decisions[0].update(disposition='already_tracked', proposed=None, tracked_id=key)
        elif reconsidering:
            decisions.append(decision(['late'], [], 'needs_evidence', 'The reported behavior is unknown.'))
        else:
            for member in ('unknown', 'late'):
                if member in member_ids:
                    decisions.append(decision([member], [], 'needs_evidence', 'The reported behavior is unknown.'))
            if 'hold' in member_ids:
                decisions.append(decision(['hold'], [], 'held', 'The evidence is unavailable for release.'))
            if 'mismatch' in member_ids:
                # Exercise the kernel's digest guard even if the mock independent verifier passes this group.
                mismatch = identity(b'mismatched-attachment')
                decisions.append(decision(['mismatch'], [mismatch], 'verified', 'An unrelated claim.',
                                          citations[0], {'title':'Unrelated claim', 'body':'Do not publish.'}))
        assessment = dict(schema='af/backlog-assessment@2', batch_id=batch['reference']['batch_id'],
                          source_revision=batch['source_revision'], decisions=decisions)
        body = '```json\n' + json.dumps(assessment) + '\n```'
        block = ('Blue was reconsidered. Needs evidence: late.' if reconsidering else
                 'Blue: blue-a, blue-b. Green: green. Needs evidence: unknown. Held: hold.')
        if duplicate:
            block = 'Late report matches the verified Green revision.'
        if not count:
            block = 'No report members. Publish only previously verified revisions.'
        elif provider_index:
            block = 'Assess the frozen members using the captured GitHub observation.'
        if not citations:
            citations.append({'path':'CONTRIBUTING.md', 'line':1})
        issue = sources['backlog-batch']
        if 'document' in inputs:
            stage = 'verifier'
            assert inputs['checks']['payload']['outcome'] == 'passed'
            document = inputs['document']['payload']['text']
            assert block in document
            if provider_index and 'blue-a' in member_ids:
                assert quote in document and '1073' in document
                assert 'already_tracked' in document and 'Private index detail.' not in document
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
        let ambient = self.root.join("fake-github/ambient-gitconfig");
        if ambient.exists() {
            command
                .env("GH_CONFIG_DIR", ambient.parent().unwrap())
                .env("GH_TOKEN", "fixture_token")
                .env("AF_BACKLOG_GITHUB_TOKEN", "ambient_wrong_token")
                .env("GIT_CONFIG_GLOBAL", &ambient);
        }
        if self.root.join("fake-jira").exists() {
            command
                .env("AF_BACKLOG_JIRA_CREDENTIAL", "ambient_wrong_credential")
                .env("AF_BACKLOG_JIRA_SCOPE", "ambient_wrong_scope");
        }
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
    private_write(
        &state.join("hosts.yml"),
        "github.com:\n  oauth_token: fixture_token\n",
    );
    private_write(
        &state.join("ambient-gitconfig"),
        "[credential]\n helper = !echo username=fixture && echo password=fixture_token && :\n",
    );
    let adapter = include_str!("../../../../../scripts/project-backlog-github.py");
    let http = include_str!("../../../../../fixtures/project-backlog/fake-github-http.py").replace(
        "__AF_BACKLOG_FIXTURE_STATE__",
        &serde_json::to_string(&state).unwrap(),
    );
    let (implementation, entrypoint) = adapter.split_once("\nif __name__ == ").unwrap();
    format!("{implementation}\n{http}\nif __name__ == {entrypoint}")
}

fn capture_backlog_batch(receiver: &Receiver, github: Option<&str>, accept_late: bool) -> Value {
    capture_backlog_intake(
        receiver,
        github.map(BacklogFixture::GitHub),
        accept_late,
        false,
    )
}

enum BacklogFixture<'a> {
    GitHub(&'a str),
    Markdown(&'a Path),
    Jira(&'a str),
}

fn capture_backlog_intake(
    receiver: &Receiver,
    fixture: Option<BacklogFixture<'_>>,
    accept_late: bool,
    bulk: bool,
) -> Value {
    let (accept, count) = configure_backlog_intake(receiver, fixture, bulk);
    let audit = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", "1772755200000"]));
    assert_eq!(audit["high_water"], count.to_string());
    if accept_late {
        accept("late");
    }
    json!({
        "schema":"af/backlog-batch-reference@1",
        "project_id":audit["project_id"],
        "batch_id":audit["active_batch_id"],
        "policy_id":audit["pin_id"],
        "high_water":audit["high_water"]
    })
}

fn configure_backlog_intake<'a>(
    receiver: &'a Receiver,
    fixture: Option<BacklogFixture<'_>>,
    bulk: bool,
) -> (impl Fn(&str) -> Value + 'a, usize) {
    configure_backlog_intake_with_filter(receiver, fixture, bulk, |_| true)
}

fn configure_backlog_intake_with_filter<'a>(
    receiver: &'a Receiver,
    fixture: Option<BacklogFixture<'_>>,
    bulk: bool,
    initial: impl Fn(&str) -> bool,
) -> (impl Fn(&str) -> Value + 'a, usize) {
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
    let record = |repo: &Path, id: &str, observed: &str, evidence_text: Option<&str>| {
        let mut args = vec![
            "backlog",
            "record",
            "--report-id",
            id,
            "--title",
            "Boundary issue",
            "--observed",
            observed,
            "--json",
        ];
        if let Some(text) = evidence_text {
            fs::write(&evidence, text).unwrap();
            args.extend(["--evidence-file", evidence.to_str().unwrap()]);
        }
        if id == "unknown" {
            args.extend(["--repro", "private-insufficient-repro"]);
        }
        ok(receiver.af_in(repo, &args))
    };
    let record_boundary =
        |repo: &Path, id, evidence| record(repo, id, "Different behavior at a boundary", evidence);
    let ids = if bulk {
        (0..131)
            .map(|index| {
                let id = format!("bulk-{index:03}");
                let boundary = match index {
                    0..64 => index,
                    64..66 => index - 64,
                    _ => index - 2,
                };
                let path = format!("bulk-{boundary:03}.py");
                let evidence = fs::read_to_string(receiver.repo.join(&path)).unwrap();
                let observed = format!("{path} rejects its required boundary {boundary}.");
                record(&receiver.repo, &id, &observed, Some(&evidence));
                id
            })
            .collect::<Vec<_>>()
    } else {
        let blue = fs::read_to_string(receiver.repo.join("blue-boundary.py")).unwrap();
        let green = fs::read_to_string(receiver.repo.join("green-boundary.py")).unwrap();
        let blue_a = record_boundary(&receiver.repo, "blue-a", Some(&blue));
        let blue_b = record_boundary(&producer_repo, "blue-b", Some(&blue));
        assert_eq!(
            record_boundary(&producer_repo, "blue-b", Some(&blue)),
            blue_b
        );
        for receipt in [&blue_a, &blue_b] {
            assert_eq!(receipt["view"], "capture");
            assert_eq!(receipt["data"]["local_capture"], true);
            assert!(receipt["data"]["authority_receipt"].is_null());
        }
        assert_eq!(blue_a["data"]["project_id"], blue_b["data"]["project_id"]);
        let private_report = |id| ok(receiver.af(&["backlog", "show", id, "--private", "--json"]));
        let a = private_report("blue-a");
        let b = private_report("blue-b");
        assert_ne!(a["data"]["workspace_id"], b["data"]["workspace_id"]);
        assert_eq!(a["data"]["evidence"], b["data"]["evidence"]);
        assert_eq!(a["data"]["privacy"], "private");
        assert_eq!(b["data"]["privacy"], "private");
        if matches!(fixture, Some(BacklogFixture::GitHub(_))) {
            eprintln!("Private captures: {blue_a} {blue_b}");
        }
        record_boundary(&receiver.repo, "green", Some(&green));
        record_boundary(&producer_repo, "hold", Some("private-attachment"));
        record_boundary(&receiver.repo, "unknown", None);
        record_boundary(&receiver.repo, "mismatch", Some("mismatched-attachment"));
        record_boundary(
            &receiver.repo,
            "late",
            (!matches!(fixture, Some(BacklogFixture::GitHub(_)))).then_some(green.as_str()),
        );
        ["blue-a", "blue-b", "green", "unknown", "hold", "mismatch"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
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
    let config = match fixture {
        Some(BacklogFixture::GitHub(executable)) => Some((
            GITHUB_CONFIG,
            json!({
                "schema": "af/backlog-github-destination@1",
                "project_id": project_id,
                "repository": "fixture/backlog",
                "repository_id": 23,
                "publisher_id": 42,
                "executable_digest": af_store::canonical::blob_content_id(executable.as_bytes()),
            }),
        )),
        Some(BacklogFixture::Markdown(path)) => Some((
            "backlog-markdown-destination-v2.json",
            json!({
                "schema": "af/backlog-markdown-destination@2", "project_id": project_id,
                "scope": "unversioned_file", "path": path,
            }),
        )),
        Some(BacklogFixture::Jira(executable)) => Some((
            "backlog-jira-destination-v1.json",
            json!({
                "schema": "af/backlog-jira-destination@1", "project_id": project_id,
                "site": "fixture.atlassian.net", "jira_project_id": "10001", "issue_type_id": "10002",
                "publisher_account_id": "fixture:publisher",
                "executable_digest": af_store::canonical::blob_content_id(executable.as_bytes()),
            }),
        )),
        None => None,
    };
    let destination = if let Some((name, config)) = config {
        if name != GITHUB_CONFIG {
            schemas::valid(&schemas::validator(name), &config);
        }
        private_write(&store.join(name), serde_json::to_vec(&config).unwrap());
        af_store::canonical::content_id(&config).unwrap()
    } else {
        af_project::backlog::batch::local_destination(project_id).unwrap()
    };
    save(
        &store.join("backlog-batch-policy-v2.json"),
        &json!({
            "schema":"af/backlog-batch-policy@2", "project_id":project_id,
            "authority_id":af_store::canonical::blob_content_id(&authority.pk.to_bytes()),
            "scenario_id":af_scenario::contract::digest(&artifact),
            "destination_id":destination,
            "timezone":"UTC", "hour":0, "minute":0,
            "start_unix_ms":1772755200000_u64, "lateness_ms":60000,
            "run_tokens":150000, "run_wall_ms":1230000, "run_dispatches":dispatches,
            "rolling_tokens":150000, "rolling_window_ms":86400000,
            "publication_limit":if matches!(fixture, Some(BacklogFixture::Markdown(_))) { 2 } else { 1 }, "evidence_access":"private_report", "hold_policy":"allow_local_reservations"
        }),
    );
    let accept = move |id: &str| {
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
        ]))
    };
    let ids: Vec<_> = ids.iter().filter(|id| initial(id)).collect();
    let first = ids.first().unwrap();
    let accepted = accept(first);
    assert_eq!(accept(first), accepted);
    let receipt_file = receiver.root.join("authority-receipt.json");
    save(&receipt_file, &accepted);
    let ack = ok(receiver.af(&["backlog", "ack", receipt_file.to_str().unwrap(), "--json"]));
    assert_eq!(ack["view"], "acknowledged");
    assert!(ack["data"]["authority_receipt"].is_string());
    for id in &ids[1..] {
        accept(id);
    }
    assert_eq!(
        ok(receiver.af(&["backlog", "list", "--json"]))["data"]["items"],
        json!([])
    );
    if matches!(fixture, Some(BacklogFixture::GitHub(_))) {
        assert!(!receiver.root.join("fake-github/requests").exists());
    }
    (accept, ids.len())
}

#[test]
fn portable_backlog_scheduler_enrolls_once_and_replays_two_daily_batches() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap().join("recurring");
    let receiver = Receiver::for_artifact(root, 303, BACKLOG);
    let (accept, first_count) = configure_backlog_intake(&receiver, None, false);
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let project_store = Path::new(project["store"].as_str().unwrap());
    let policy_file = project_store.join("backlog-batch-policy-v2.json");
    let due = af_scenario::runtime::now().unwrap() / 60_000 * 60_000;
    let mut policy = read(&policy_file);
    policy["start_unix_ms"] = json!(due);
    policy["hour"] = json!(due / 3_600_000 % 24);
    policy["minute"] = json!(due / 60_000 % 60);
    policy["lateness_ms"] = json!(600_000);
    save(&policy_file, &policy);

    let state = receiver.scenarios.to_str().unwrap();
    let before = (due - 1).to_string();
    let receiver_args = [
        "--file",
        BACKLOG,
        "--task-state",
        receiver.tasks.to_str().unwrap(),
        "--owner",
        "fixture-owner",
        "--state",
        state,
        "--now",
        &before,
        "--json",
    ];
    let enroll = |operation, extra: &[&str]| {
        receiver.af(&[&["ant", "backlog", operation][..], &receiver_args, extra].concat())
    };
    let preview = ok(enroll("validate", &[]));
    schemas::valid(&schemas::validator("backlog-schedule-v2.json"), &preview);
    let wrong = enroll("enroll", &["--confirm-pin", "wrong"]);
    assert!(!wrong.status.success() && text(&wrong).contains("confirmation differs"));
    assert!(!receiver.provider_home.join("calls").exists());
    let confirmed = ["--confirm-pin", preview["revision"].as_str().unwrap()];
    assert_eq!(ok(enroll("enroll", &confirmed)), preview);
    assert_eq!(ok(enroll("enroll", &confirmed)), preview);
    let manual = receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", &due.to_string()]);
    assert!(!manual.status.success() && text(&manual).contains("enrolled scheduler"));

    let registry = receiver.root.join("providers.toml");
    let registry_bytes = fs::read(&registry).unwrap();
    fs::write(&registry, [&registry_bytes[..], b"\n# drift\n"].concat()).unwrap();
    let refused = ok(receiver.af(&[
        "ant",
        "tick",
        "--state",
        state,
        "--now",
        &due.to_string(),
        "--json",
    ]));
    assert!(
        refused["refused"]
            .as_object()
            .unwrap()
            .values()
            .any(|error| {
                error
                    .as_str()
                    .unwrap()
                    .contains("Provider registry or bindings changed")
            })
    );
    assert_eq!(
        ok(receiver.af(&["backlog", "batch", "audit"]))["occurrences"],
        json!([])
    );
    assert!(!receiver.provider_home.join("calls").exists());
    fs::write(&registry, registry_bytes).unwrap();

    let sink = project_store.join("backlog-local-sink");
    fs::write(&sink, "temporary sink obstruction").unwrap();
    // Drive the first slot through the real foreground scheduler. The local sink fails
    // after independent verification and durable settlement, leaving a recoverable intent.
    let scheduler = receiver.af(&[
        "ant",
        "scheduler",
        "run",
        "--state",
        state,
        "--for-seconds",
        "1",
    ]);
    assert!(scheduler.status.success(), "{}", text(&scheduler));
    let runs =
        || ok(receiver.af(&["scenario", "list", "--state", state, "--json"]))["runs"].clone();
    let first = runs();
    let (first_id, first_run) = first.as_object().unwrap().iter().next().unwrap();
    assert_eq!(first.as_object().unwrap().len(), 1);
    assert_eq!(first_run["status"], "waiting", "{first_run}");
    let first_steps = &first_run["frames"]["root"]["steps"];
    assert_eq!(first_steps[0]["status"], "succeeded");
    assert_eq!(first_steps[1]["status"], "succeeded");
    assert_eq!(first_steps[2]["status"], "waiting");
    let audit = || ok(receiver.af(&["backlog", "batch", "audit"]));
    let first_batch = audit()["active_batch_id"].as_str().unwrap().to_owned();
    assert_eq!(
        audit()["batches"][&first_batch]["members"]
            .as_object()
            .unwrap()
            .len(),
        first_count
    );
    accept("late");
    assert_eq!(audit()["pending_member_ids"], json!(["late"]));
    assert!(audit()["batches"][&first_batch]["members"]["late"].is_null());

    let tick = |now: u64| {
        ok(receiver.af(&[
            "ant",
            "tick",
            "--state",
            state,
            "--now",
            &now.to_string(),
            "--json",
        ]))
    };
    let observed = af_scenario::runtime::now().unwrap();
    // A policy edit must block both new admission and continuation of the already verified
    // Run. Restoring the exact policy permits reconciliation of the original intent.
    let mut changed = policy.clone();
    changed["publication_limit"] = json!(2);
    save(&policy_file, &changed);
    fs::remove_file(&sink).unwrap();
    let blocked = tick(observed);
    assert!(
        blocked["refused"][first_id]
            .as_str()
            .unwrap()
            .contains("installed Project authority")
    );
    assert_eq!(&runs()[first_id], first_run);
    assert!(!sink.exists());
    save(&policy_file, &policy);

    let recovered = tick(observed);
    assert_eq!(recovered["started"], json!([]));
    let first_done = &recovered["runs"][first_id];
    assert_eq!(first_done["status"], "succeeded", "{first_done}");
    assert_eq!(
        first_done["frames"]["root"]["steps"][2]["intents"][0]["id"],
        first_steps[2]["intents"][0]["id"],
    );
    assert_eq!(fs::read_dir(&sink).unwrap().count(), 1);
    let closed = audit()["batches"][&first_batch]["closed_unix_ms"]
        .as_u64()
        .unwrap();
    let second_time = closed + 86_400_000;
    assert!(second_time < due + 86_400_000 + 600_000);
    let second = tick(second_time);
    let second_id = second["started"][0].as_str().unwrap();
    let second_done = &second["runs"][second_id];
    assert_eq!(second_done["status"], "succeeded", "{second_done}");
    assert_ne!(first_id, second_id);
    assert_ne!(
        first_done["pin"]["revision"],
        second_done["pin"]["revision"]
    );
    assert_ne!(
        first_done["pin"]["policy"]["task_templates"],
        second_done["pin"]["policy"]["task_templates"]
    );
    let audits = audit();
    let admitted = audits["occurrences"].as_array().unwrap();
    assert_eq!(admitted.len(), 2);
    let second_batch = admitted[1]["outcome"]["batch_id"].as_str().unwrap();
    assert_eq!(admitted[0]["outcome"]["batch_id"], first_batch);
    assert_ne!(first_batch, second_batch);
    assert_eq!(
        audits["batches"][second_batch]["members"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["late"]
    );
    assert_eq!(audits["closed_high_water"], (first_count + 1).to_string());
    assert_eq!(fs::read_dir(&sink).unwrap().count(), 2);

    let cas = Cas::open_existing(receiver.tasks.join("cas")).unwrap();
    let mut task_receipts = Vec::new();
    let mut reviews = Vec::new();
    for (run, batch_id) in [
        (first_done, first_batch.as_str()),
        (second_done, second_batch),
    ] {
        let requirements = &run["pin"]["bundle"]["definitions"]["daily-backlog"]["steps"][0]["operation"]
            ["template"]["document"]["requirements"];
        assert_eq!(
            requirements["scenario_backlog_enrollment"],
            preview["revision"]
        );
        let outputs = &run["frames"]["root"]["outputs"];
        let task = &outputs["assessment"]["value"];
        assert_eq!(task["result"]["acceptance"], "satisfied");
        task_receipts.push(task["result_id"].clone());
        let verification = task["result"]["outputs"]["verification"]["artifact_ids"][0]
            .as_str()
            .unwrap();
        let review = cas.get_artifact(verification).unwrap();
        assert_eq!(review.payload["outcome"], "passed");
        reviews.push(review.payload["evaluation_id"].as_str().unwrap().to_owned());
        assert_eq!(outputs["settle-backlog"]["value"]["subject"], batch_id);
        assert_eq!(outputs["dispatch-backlog"]["value"]["subject"], batch_id);
        assert!(outputs["dispatch-backlog"]["value"]["evidence"].is_string());
    }
    assert_ne!(task_receipts[0], task_receipts[1]);
    assert_ne!(reviews[0], reviews[1]);
    let calls = fs::read_to_string(receiver.provider_home.join("calls")).unwrap();
    assert_eq!(calls.lines().filter(|line| *line == "verifier").count(), 2);
    assert_eq!(tick(second_time)["started"], json!([]));
    assert_eq!(tick(second_time + 1)["runs"], json!({}));
    assert_eq!(audit(), audits);
    assert_eq!(fs::read_dir(&sink).unwrap().count(), 2);

    let pause_at = (second_time + 2).to_string();
    let mode =
        |operation, time| ok(receiver.af(&["ant", "backlog", operation, "--now", time, "--json"]));
    assert_eq!(mode("pause", &pause_at)["paused"], true);
    let third_due = (due + 2 * 86_400_000).to_string();
    // Resume without an intervening tick must preserve the skipped paused occurrence.
    assert_eq!(mode("resume", &third_due)["paused"], false);
    assert_eq!(
        audit()["occurrences"][2]["outcome"]["kind"],
        "skipped_paused"
    );
    assert_eq!(tick(due + 3 * 86_400_000 + 600_001)["started"], json!([]));
    assert_eq!(
        audit()["occurrences"][3]["outcome"]["kind"],
        "skipped_missed"
    );
    assert_eq!(
        fs::read_to_string(receiver.provider_home.join("calls")).unwrap(),
        calls
    );
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
    for issue in [303, 404, 505] {
        let receiver = Receiver::for_artifact(parent.join(issue.to_string()), issue, BACKLOG);
        let reference = capture_backlog_batch(&receiver, None, issue != 303);
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
            &schemas::validator("backlog-collected-batch-v3.json"),
            &collected,
        );
        assert_eq!(collected["reference"], reference);
        assert_eq!(collected["members"].as_array().unwrap().len(), 6);
        assert!(collection.to_string().contains("private-attachment"));
        assert!(collection.to_string().contains("Different behavior"));
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
                    let record: af_contracts::backlog::BacklogRecordV2 =
                        serde_json::from_value(event.payload).unwrap();
                    match record {
                        af_contracts::backlog::BacklogRecordV2::Publication(value) => Some(value),
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
        if issue != 303 {
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
            &schemas::validator("backlog-assessment-v2.json"),
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
        assert_backlog_publication_conformance(
            &receiver,
            store_path,
            &json!(af_contracts::backlog::BacklogRecordV2::Publication(
                settled[1].clone()
            )),
        );
        assert_eq!(
            settled[1].receipt_id.as_deref(),
            Some(af_store::canonical::blob_content_id(&fs::read(&entry).unwrap()).as_str())
        );
        let audit = ok(receiver.af(&["backlog", "batch", "audit"]));
        assert!(audit["active_batch_id"].is_null());
        assert_eq!(audit["closed_high_water"], "6");
        let late = if issue != 303 {
            json!(["late"])
        } else {
            json!([])
        };
        assert_eq!(audit["pending_member_ids"], late);
        let green_revision = green["revision_id"].as_str().unwrap();
        assert!(audit["pending_deliveries"][green_revision]["decision_id"].is_string());
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

        let closed = audit["batches"][reference["batch_id"].as_str().unwrap()]["closed_unix_ms"]
            .as_u64()
            .unwrap();
        let due = ((closed / 86_400_000 + 2) * 86_400_000).to_string();
        if issue == 505 {
            let policy_path = store_path.join("backlog-batch-policy-v2.json");
            let mut policy = read(&policy_path);
            policy["publication_limit"] = json!(2);
            save(&policy_path, &policy);
        }
        let next = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", &due]));
        assert_eq!(next["pin_id"] == audit["pin_id"], issue != 505);
        let later = json!({
            "schema":"af/backlog-batch-reference@1", "project_id":reference["project_id"],
            "batch_id":next["active_batch_id"], "policy_id":next["pin_id"], "high_water":next["high_water"],
        });
        let later_members = &next["batches"][later["batch_id"].as_str().unwrap()]["members"];
        assert_eq!(
            later_members.as_object().unwrap().len(),
            usize::from(issue != 303)
        );
        let (id, resumed) = start_backlog_ant(&receiver, "continued-backlog", &later);
        assert_eq!(resumed["status"], "succeeded", "{}", resumed["failure"]);
        let later_audit = ok(receiver.af(&["backlog", "batch", "audit"]));
        assert_eq!(later_audit["pending_deliveries"], json!({}));
        assert_eq!(
            later_audit["delivery_resolutions"][green_revision]["outcome"],
            if issue == 505 {
                "requires_reverification"
            } else {
                "delivered"
            }
        );
        let delivered = publications();
        if issue == 505 {
            assert_eq!(fs::read_dir(&sink).unwrap().count(), 1);
            assert_eq!(delivered, settled);
        } else {
            assert_eq!(fs::read_dir(&sink).unwrap().count(), 2);
            assert_eq!(delivered.len(), 4);
            assert_eq!(delivered[3].revision_id, green_revision);
            assert_eq!(
                delivered[3].decision_id,
                audit["pending_deliveries"][green_revision]["decision_id"]
            );
        }
        if issue != 303 {
            let duplicate = private_outcome("late");
            assert_eq!(duplicate["disposition"], "already_tracked");
            assert_eq!(
                duplicate["local_match"],
                json!({
                    "item_id":green["item_id"], "revision_id":green_revision
                })
            );
            assert_eq!(ok(receiver.af(&["backlog", "list", "--json"])), visible);
        }
        let replayed = ok(receiver.af(&[
            "scenario",
            "resume",
            &id,
            "--state",
            receiver.scenarios.to_str().unwrap(),
            "--json",
        ]));
        assert_eq!(replayed, resumed);
        assert_eq!(publications(), delivered);
    }
    assert_ne!(receipts[0], receipts[1]);
}

#[test]
fn portable_backlog_bounds_distinct_citations_and_holds_an_omitted_duplicate() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("bulk"),
        129,
        BACKLOG,
    );
    for boundary in 0..129 {
        fs::write(
            receiver.repo.join(format!("bulk-{boundary:03}.py")),
            format!("def accepts(value):\n    return value > {boundary}\n"),
        )
        .unwrap();
    }
    capture(&receiver.repo);
    let mut reference = capture_backlog_intake(&receiver, None, false, true);
    let audit = || ok(receiver.af(&["backlog", "batch", "audit"]));
    let outcome = |id: &str| {
        ok(receiver.af(&["backlog", "show", id, "--private", "--json"]))["data"]["outcomes"][0]
            .clone()
    };
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store = Path::new(project["store"].as_str().unwrap());
    let sink = store.join("backlog-local-sink");
    let mut assessed = 0;
    let mut first_batch_count = 0;
    for index in 0..3 {
        let state = audit();
        let batch = reference["batch_id"].as_str().unwrap();
        let count = state["batches"][batch]["members"]
            .as_object()
            .unwrap()
            .len();
        assert!((1..=64).contains(&count));
        if index == 0 {
            first_batch_count = count;
        }
        assessed += count;
        let (_, run) = start_backlog_ant(&receiver, &format!("bulk-backlog-{index}"), &reference);
        assert_eq!(
            run["status"], "succeeded",
            "batch {index}: {}",
            run["failure"]
        );
        let closed = audit();
        assert!(closed["active_batch_id"].is_null());
        assert_eq!(
            closed["pending_member_ids"].as_array().unwrap().len(),
            131 - assessed
        );
        assert_eq!(fs::read_dir(&sink).unwrap().count(), index + 1);
        let list = ok(receiver.af(&["backlog", "list", "--json"]));
        assert_eq!(
            list["data"]["items"].as_array().unwrap().len(),
            first_batch_count
        );
        if index == 2 {
            assert_eq!(assessed, 131);
            break;
        }
        let closed_at = closed["batches"][batch]["closed_unix_ms"].as_u64().unwrap();
        let due = ((closed_at / 86_400_000 + 2) * 86_400_000).to_string();
        let next = ok(receiver.af(&["backlog", "batch", "tick", "--at-unix-ms", &due]));
        reference["batch_id"] = next["active_batch_id"].clone();
    }
    // The first two verified revisions cannot both fit in 32 KiB. A duplicate of
    // an omitted revision stays private, even with a digest-matched repository file.
    let duplicates = [outcome("bulk-064"), outcome("bulk-065")];
    assert!(
        duplicates
            .iter()
            .any(|decision| decision["disposition"] == "held")
    );
    assert!(duplicates.iter().all(|decision| {
        matches!(
            decision["disposition"].as_str(),
            Some("held" | "already_tracked")
        )
    }));
    assert_eq!(outcome("bulk-130")["disposition"], "held");
}

fn start_backlog_ant(receiver: &Receiver, id: &str, batch: &Value) -> (String, Value) {
    let ant_path = receiver.root.join("ant.json");
    let policy_path = receiver.root.join("policy.json");
    if !policy_path.exists() {
        private_write(&policy_path, "{}");
    }
    let state_args = ["--state", receiver.scenarios.to_str().unwrap(), "--json"];
    let definition_args = [
        "--file",
        ant_path.to_str().unwrap(),
        "--authority",
        policy_path.to_str().unwrap(),
    ];
    let ant = |args: &[&str]| ok(receiver.af(&[&["ant"][..], args, &state_args].concat()));
    save(&receiver.input, batch);
    let bundle = ok(receiver.prepare_artifact(BACKLOG, None));
    let root = &bundle["definitions"]["daily-backlog"];
    let limits = root["limits"].clone();
    let due = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 60
        * 60_000;
    let hour = due / 3_600_000 % 24;
    let minute = due / 60_000 % 60;
    let before_due = (due - 1).to_string();
    save(
        &policy_path,
        &json!({
            "schema":"af/scenario-policy@8",
            "task_plans":[], "task_templates":root["authority"]["task_templates"],
            "adapters":{}, "decision_keys":{}, "file_roots":[]
        }),
    );
    save(
        &ant_path,
        &json!({
            "schema":"af/ant@12", "id":id, "scenario":bundle, "inputs":{},
            "schedule":{"kind":"daily","timezone":"UTC","hour":hour,"minute":minute,"start_unix_ms":due - 1},
            "owner":"fixture-owner", "notification":"local:fixture-owner",
            "execution":{"overlap":"skip","missed":"skip","lateness_ms":120000,"per_run":limits,
                         "rolling_tokens":150000,"rolling_window_ms":86400000}
        }),
    );
    let pin = ant(&[&["validate"][..], &definition_args].concat());
    ant(&[
        &["add"][..],
        &definition_args,
        &[
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
            "--now",
            &before_due,
        ],
    ]
    .concat());
    ant(&["enable", id, "--now", &before_due]);
    // Use the same real clock for the dispatch deadline on tick and on resume.
    let tick = ant(&["tick"]);
    let shown = ant(&["show", id]);
    let occurrences = shown["occurrences"].as_array().unwrap();
    assert_eq!(occurrences.len(), 1);
    let run_id = occurrences[0]["outcome"]["run_id"].as_str().unwrap();
    assert_eq!(tick["started"], json!([run_id]));
    (run_id.to_owned(), tick["runs"][run_id].clone())
}

fn http_requests(fake: &Path) -> Vec<Value> {
    fs::read_to_string(fake.join("requests"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn configure_backlog_receiver_fault_bounds(receiver: &Receiver) {
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
    use af_contracts::backlog::{BacklogRecordV2, Publication};

    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        505,
        BACKLOG,
    );
    configure_backlog_receiver_fault_bounds(&receiver);
    let executable = github_fixture(&receiver);
    let reference = capture_backlog_batch(&receiver, Some(&executable), true);
    let (run_id, first) = start_backlog_ant(&receiver, "github-backlog", &reference);
    let resume = [
        "scenario",
        "resume",
        &run_id,
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
                match serde_json::from_value::<BacklogRecordV2>(event.payload).unwrap() {
                    BacklogRecordV2::Publication(value) => {
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
    assert_eq!(first["status"], "waiting", "{}", first["failure"]);
    assert_eq!(first["frames"]["root"]["steps"][1]["status"], "succeeded");
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
    let credential = store_path.join("backlog-github-token");
    assert!(!credential.exists());
    private_write(&adapter, "unpinned bytes");
    check_pending(pending());
    assert!(!fake.join("requests").exists());
    fs::remove_file(&adapter).unwrap();
    private_write(&adapter, &executable);
    private_write(&credential, "fixture_token\n");
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
    human["title"] = json!("Operator title");
    human["body"] = json!(human["body"].as_str().unwrap().replace(
        "The configured boundary must be inclusive.",
        "Operator body"
    ));
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
    let run = ok(receiver.af(&resume));
    assert_eq!(run["status"], "succeeded");

    let history = publications();
    let final_record = history.last().unwrap();
    assert_eq!(final_record.intent_id, intents[0].intent_id);
    assert_eq!(final_record.remote_id.as_deref(), Some("444:1"));
    assert!(final_record.receipt_id.is_some());
    assert_eq!(final_record.attempt_ids.len(), 6);
    assert_backlog_publication_conformance(
        &receiver,
        store_path,
        &json!(BacklogRecordV2::Publication(final_record.clone())),
    );
    let observed = state(final_record);
    assert_eq!(observed["outcome"]["state"], "closed");
    assert_eq!(observed["outcome"]["approved_fields_match"], false);
    assert_eq!(read(&fake.join("issue.json")), human);
    let requests = http_requests(&fake);
    let writes: Vec<_> = requests
        .iter()
        .filter(|call| call["method"] == "POST")
        .collect();
    assert_eq!(writes.len(), 1);
    let marker = format!(
        "<!-- af-backlog-item:{}:{} -->",
        final_record.project_id, final_record.item_id
    );
    assert!(
        writes[0]["body"]["body"]
            .as_str()
            .unwrap()
            .ends_with(&marker)
    );
    assert_eq!(final_record.project_id, reference["project_id"]);
    let config = read(&store_path.join(GITHUB_CONFIG));
    assert_eq!(
        final_record.destination_id,
        af_store::canonical::content_id(&config).unwrap()
    );
    assert_eq!(
        ok(receiver.af(&["backlog", "show", "unknown", "--private", "--json"]))["data"]["outcomes"]
            [0]["disposition"],
        "needs_evidence"
    );

    // A later independently verified revision reconciles the same immutable issue and preserves
    // the operator's edit and closure. Move to a new source so the deferred green item needs proof.
    let audit = ok(receiver.af(&["backlog", "batch", "audit"]));
    assert_eq!(audit["intake"].as_array().unwrap().len(), 7);
    assert_eq!(audit["closed_high_water"], "6");
    assert_eq!(audit["pending_member_ids"], json!(["late"]));
    let member = |id| ok(receiver.af(&["backlog", "show", id, "--private", "--json"]));
    let blue_a = member("blue-a");
    let blue_b = member("blue-b");
    assert_eq!(blue_a["data"]["outcomes"], blue_b["data"]["outcomes"]);
    assert_eq!(
        blue_a["data"]["outcomes"][0]["item_id"],
        final_record.item_id
    );
    assert_eq!(
        blue_a["data"]["outcomes"][0]["revision_id"],
        final_record.revision_id
    );
    let insufficient = member("unknown");
    assert_eq!(
        insufficient["data"]["report"]["repro"],
        "private-insufficient-repro"
    );
    assert!(!human.to_string().contains("private-insufficient-repro"));
    let held = member("hold");
    assert_eq!(held["data"]["outcomes"][0]["disposition"], "held");
    let mismatch = member("mismatch");
    assert_eq!(mismatch["data"]["outcomes"][0]["disposition"], "held");
    let batch = &audit["batches"][reference["batch_id"].as_str().unwrap()];
    assert_eq!(batch["members"]["green"]["disposition"], "accounted");
    let green = member("green");
    let green_outcome = &green["data"]["outcomes"][0];
    assert_eq!(green_outcome["disposition"], "verified");
    assert!(
        history
            .iter()
            .all(|record| record.item_id != green_outcome["item_id"])
    );
    let assessment = &run["frames"]["root"]["outputs"]["assessment"]["value"];
    assert_eq!(assessment["result"]["acceptance"], "satisfied");
    eprintln!(
        "Offline cutover proof: {}",
        json!({
            "run_id": run_id, "batch_id": reference["batch_id"],
            "reports": ["blue-a", "blue-b"], "frozen_reports": 6, "verified_items": 2,
            "already_tracked": 0, "tickets_before_verification": 0,
            "ticket_writes": writes.len(), "item_id": final_record.item_id,
            "revision_id": final_record.revision_id, "intent_id": final_record.intent_id,
            "receipt_id": final_record.receipt_id, "remote_id": final_record.remote_id,
            "assessment_task": assessment["task_id"],
            "charged_tokens": assessment["charged_tokens"],
            "verification": assessment["result"]["outputs"]["verification"]["artifact_ids"],
            "held": ["hold", "mismatch"], "insufficient": "unknown",
            "deferred_publication": green_outcome["item_id"],
            "after_cutoff": audit["pending_member_ids"], "accounting": batch["members"],
        })
    );
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
    fs::remove_file(&credential).unwrap();
    let (revised_run, waiting) =
        start_backlog_ant(&receiver, "revised-github-backlog", &revised_reference);
    assert_eq!(waiting["status"], "waiting");
    assert_eq!(http_requests(&fake), requests);
    private_write(&credential, "fixture_token\n");
    let revised_result = ok(receiver.af(&[
        "scenario",
        "resume",
        &revised_run,
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ]));
    assert_eq!(
        revised_result["status"], "succeeded",
        "{}",
        revised_result["failure"]
    );
    let after = ok(receiver.af(&["backlog", "batch", "audit"]));
    let green_revision = green_outcome["revision_id"].as_str().unwrap();
    assert_eq!(after["pending_deliveries"], json!({}));
    assert_eq!(
        after["delivery_resolutions"][green_revision]["outcome"],
        "requires_reverification"
    );
    let revised_history = publications();
    let revised = revised_history.last().unwrap();
    assert_eq!(revised.item_id, final_record.item_id);
    assert_ne!(revised.revision_id, final_record.revision_id);
    assert_eq!(revised.remote_id, final_record.remote_id);
    assert!(revised.receipt_id.is_some());
    assert_ne!(revised.receipt_id, final_record.receipt_id);
    assert_eq!(revised.attempt_ids.len(), 1);
    assert_eq!(state(revised)["operation"], "reconcile");
    assert_eq!(state(revised)["outcome"]["state"], "closed");
    assert_eq!(state(revised)["outcome"]["approved_fields_match"], false);
    let final_requests = http_requests(&fake);
    let later = &final_requests[requests.len()..];
    assert_eq!(later.len(), 3);
    assert!(
        later
            .iter()
            .all(|call| call["method"] == "GET" && call["body"].is_null())
    );
    assert_eq!(later[2]["path"], "/repos/fixture/backlog/issues/1");
    println!(
        "Project {} item {} destination {} virtual issue {} first revision {} attempts {:?} receipt {}, later revision {} attempts {:?} receipt {}, added visible issues on retry and later revision 0",
        revised.project_id,
        revised.item_id,
        revised.destination_id,
        revised.remote_id.as_deref().unwrap(),
        final_record.revision_id,
        final_record.attempt_ids,
        final_record.receipt_id.as_deref().unwrap(),
        revised.revision_id,
        revised.attempt_ids,
        revised.receipt_id.as_deref().unwrap(),
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
    let reference = capture_backlog_batch(&receiver, Some(&executable), true);
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store_path = Path::new(project["store"].as_str().unwrap());
    private_write(&store_path.join(GITHUB_ADAPTER), &executable);
    let fake = receiver.root.join("fake-github");
    fs::write(fake.join("abort-before-write"), "").unwrap();
    let (run_id, first) = start_backlog_ant(&receiver, "before-write", &reference);
    let resume = [
        "scenario",
        "resume",
        &run_id,
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ];
    assert_eq!(first["status"], "waiting", "{}", first["failure"]);
    assert!(!fake.join("requests").exists());
    private_write(&store_path.join("backlog-github-token"), "fixture_token\n");
    let interrupted = ok(receiver.af(&resume));
    assert_eq!(
        interrupted["status"], "waiting",
        "{}",
        interrupted["failure"]
    );
    assert!(!fake.join("issue.json").exists());
    fs::remove_file(fake.join("abort-before-write")).unwrap();
    fs::write(fake.join("mode"), "delayed").unwrap();
    let second = ok(receiver.af(&resume));
    assert_eq!(second["status"], "waiting", "{}", second["failure"]);
    assert!(!fake.join("issue.json").exists());

    let requests = http_requests(&fake);
    assert_eq!(
        requests
            .iter()
            .filter(|call| call["method"] == "POST")
            .count(),
        1
    );
    assert_eq!(requests.last().unwrap()["method"], "GET");
    let database = af_store::EventStore::open_read_only(store_path.join("events.sqlite")).unwrap();
    let events = database.replay("backlog-processing").unwrap();
    let publication = &events.last().unwrap().payload;
    assert_eq!(publication["attempt_ids"].as_array().unwrap().len(), 2);
    assert!(publication["remote_id"].is_null());
    assert!(publication["receipt_id"].is_null());
}

fn github_observer_fixture(receiver: &Receiver) -> Vec<u8> {
    let fixture = tempfile::tempdir_in(&receiver.root).unwrap();
    let source = fixture.path().join("source");
    fs::create_dir(&source).unwrap();
    let observer = include_str!("../../../../../scripts/backlog-open-issues.py");
    let (implementation, entrypoint) = observer.split_once("\nif __name__ == ").unwrap();
    let transport = include_str!("../../../../../fixtures/project-backlog/fake-github-observer.py")
        .replace(
            "__AF_BACKLOG_FIXTURE_STATE__",
            &serde_json::to_string(&receiver.root.join("fake-github")).unwrap(),
        );
    fs::write(
        source.join("__main__.py"),
        format!("{implementation}\n{transport}\nif __name__ == {entrypoint}"),
    )
    .unwrap();
    copy_tree(
        &workspace().join("scripts/performance/perflab"),
        &source.join("performance/perflab"),
    );
    let archive = fixture.path().join("observer");
    let output = Command::new("python3")
        .args(["-I", "-m", "zipapp", "-p", "/usr/bin/env python3"])
        .arg(source)
        .arg("-o")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::read(archive).unwrap()
}

#[test]
fn portable_backlog_scheduler_indexes_fresh_github_evidence_before_each_batch() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory
            .path()
            .canonicalize()
            .unwrap()
            .join("indexed receiver"),
        513,
        BACKLOG,
    );
    let publisher = github_fixture(&receiver);
    let (accept, _) = configure_backlog_intake_with_filter(
        &receiver,
        Some(BacklogFixture::GitHub(&publisher)),
        false,
        |id| matches!(id, "blue-a" | "blue-b"),
    );
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store = Path::new(project["store"].as_str().unwrap());
    let fake = receiver.root.join("fake-github");
    let mode = |value| fs::write(fake.join("observer-mode"), value).unwrap();
    mode("first");
    let observer = github_observer_fixture(&receiver);
    private_write(&store.join("backlog-github-observer"), &observer);
    let read_token = store.join("backlog-github-read-token");
    private_write(&read_token, "fixture_issues_read_only\n");
    private_write(&store.join(GITHUB_ADAPTER), &publisher);

    let policy_file = store.join("backlog-batch-policy-v2.json");
    let due = af_scenario::runtime::now().unwrap() / 60_000 * 60_000;
    let mut policy = read(&policy_file);
    policy["start_unix_ms"] = json!(due);
    policy["hour"] = json!(due / 3_600_000 % 24);
    policy["minute"] = json!(due / 60_000 % 60);
    policy["lateness_ms"] = json!(600_000);
    save(&policy_file, &policy);
    let state = receiver.scenarios.to_str().unwrap();
    let before = (due - 1).to_string();
    let args = [
        "--file",
        BACKLOG,
        "--task-state",
        receiver.tasks.to_str().unwrap(),
        "--owner",
        "fixture-owner",
        "--state",
        state,
        "--now",
        &before,
        "--github-issue-index",
        "--json",
    ];
    let enrollment =
        |verb, extra: &[&str]| receiver.af(&[&["ant", "backlog", verb][..], &args, extra].concat());
    let preview = ok(enrollment("validate", &[]));
    schemas::valid(&schemas::validator("backlog-schedule-v2.json"), &preview);
    assert_eq!(
        preview["scope"]["github_observer"],
        af_store::canonical::blob_content_id(&observer)
    );
    let manual_pin = store.join("backlog-provider-index-v1.json");
    private_write(&manual_pin, "Existing manual pin");
    let conflict = enrollment("validate", &[]);
    assert!(!conflict.status.success() && text(&conflict).contains("manual backlog preparation"));
    assert_eq!(
        fs::read_to_string(&manual_pin).unwrap(),
        "Existing manual pin"
    );
    fs::remove_file(&manual_pin).unwrap();
    assert_eq!(
        ok(enrollment(
            "enroll",
            &["--confirm-pin", preview["revision"].as_str().unwrap()]
        )),
        preview
    );
    let reads = fake.join("observer-requests");
    assert!(!reads.exists());
    let enrollment_file = store.join("backlog-schedule-v1.json");
    let mut old = preview.clone();
    old["schema"] = json!("af/backlog-schedule@1");
    old["scope"]
        .as_object_mut()
        .unwrap()
        .remove("github_observer");
    let installed = fs::read(&enrollment_file).unwrap();
    fs::write(&enrollment_file, serde_json::to_vec(&old).unwrap()).unwrap();
    let rejected = receiver.af(&["ant", "backlog", "show", "--state", state]);
    assert!(!rejected.status.success());
    fs::write(&enrollment_file, installed).unwrap();

    let tick = |now: u64| {
        ok(receiver.af(&[
            "ant",
            "tick",
            "--state",
            state,
            "--now",
            &now.to_string(),
            "--json",
        ]))
    };
    let audit = || ok(receiver.af(&["backlog", "batch", "audit"]));
    let binding_path = |id: &str| store.join(format!("backlog-provider-index-{}.json", &id[7..]));
    let index = |run: &Value| {
        let text = run["pin"]["bundle"]["definitions"]["daily-backlog"]["steps"][0]["operation"]
            ["template"]["document"]["requirements"]["scenario_portable_sources"]["sources"]
            ["backlog-batch"]["text"].as_str().unwrap();
        let collection: Value = serde_json::from_str(text).unwrap();
        let batch = collection["reference"]["batch_id"].as_str().unwrap();
        let pin = read(&binding_path(batch));
        schemas::valid(
            &schemas::validator("backlog-provider-index-pin-v1.json"),
            &pin,
        );
        assert_eq!(pin["batch_id"], batch);
        assert_eq!(pin["source_revision"], collection["source_revision"]);
        assert_eq!(pin["destination_id"], policy["destination_id"]);
        let index = &collection["provider_index"];
        let id = index["observation_id"].as_str().unwrap();
        assert_eq!(pin["observation_id"], id);
        let raw = fs::read(store.join(format!("backlog-open-issues-{}.json", &id[7..]))).unwrap();
        assert_eq!(af_store::canonical::blob_content_id(&raw), id);
        assert!(raw.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<Value>(&raw).unwrap(),
            index["observation"]
        );
        assert_eq!(index["observation"]["repository"], "Fixture/Backlog");
        assert_eq!(index["observation"]["issues"][0]["body_truncated"], false);
        assert!(
            index["observation"]["issues"][0]["body_excerpt"]
                .as_str()
                .unwrap()
                .contains("Private index detail.")
        );
        (pin, raw)
    };
    let publish_token = store.join("backlog-github-token");
    private_write(&publish_token, "fixture_issues_read_only\n");
    let shared_credential = tick(due);
    assert!(
        shared_credential["refused"]
            .as_object()
            .unwrap()
            .values()
            .any(|error| error
                .as_str()
                .is_some_and(|error| error.contains("require distinct credentials"))),
        "{shared_credential}"
    );
    assert!(!reads.exists());
    fs::remove_file(&publish_token).unwrap();

    let first = tick(due);
    let first_id = first["started"][0]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"));
    let first_run = &first["runs"][first_id];
    assert_eq!(first_run["status"], "succeeded", "{}", first_run["failure"]);
    let (first_pin, first_raw) = index(first_run);
    assert_eq!(fs::read_to_string(&reads).unwrap().lines().count(), 3);
    assert!(!fake.join("requests").exists());
    let first_audit = audit();
    let first_batch = first_pin["batch_id"].as_str().unwrap();
    let closed = first_audit["batches"][first_batch]["closed_unix_ms"]
        .as_u64()
        .unwrap();
    let day_two = closed + 86_400_000;
    let tracked = ok(receiver.af(&["backlog", "show", "blue-a", "--private", "--json"]));
    let decision = &tracked["data"]["outcomes"][0];
    assert_eq!(decision["disposition"], "already_tracked");
    assert!(
        decision["reason"]
            .as_str()
            .unwrap()
            .contains("Verified GitHub issue ID 1073 (#73)")
    );

    accept("green");
    let project_refusal = format!(
        "project-backlog-{}",
        &project["project_id"].as_str().unwrap()[7..]
    );
    let provider_calls = receiver.provider_home.join("calls");
    let first_calls = fs::read(&provider_calls).unwrap();
    fs::remove_file(&read_token).unwrap();
    let missing = tick(day_two);
    assert!(
        missing["refused"][&project_refusal]
            .as_str()
            .unwrap()
            .contains("credential unavailable")
    );
    private_write(&read_token, "fixture_issues_read_only\n");
    let frozen = audit();
    let second_batch = frozen["active_batch_id"].as_str().unwrap();
    assert_ne!(first_batch, second_batch);
    for (failure, diagnostic) in [
        ("unavailable", "observation unavailable"),
        ("partial", "observation unavailable"),
        ("wrong-scope", "different GitHub repository"),
        ("oversize", "over-bound GitHub backlog observation"),
    ] {
        mode(failure);
        let refused = tick(day_two);
        assert_eq!(refused["started"], json!([]));
        assert!(
            refused["refused"][&project_refusal]
                .as_str()
                .unwrap()
                .contains(diagnostic),
            "{refused}"
        );
        assert_eq!(audit(), frozen);
        assert!(!binding_path(second_batch).exists());
        assert_eq!(fs::read(&provider_calls).unwrap(), first_calls);
        assert!(!fake.join("requests").exists());
    }
    // Simulate interruption between binding the observation and admitting the prepared Run.
    let registry = receiver.root.join("providers.toml");
    let registry_bytes = fs::read(&registry).unwrap();
    mode("second-with-registry-drift");
    let interrupted = tick(day_two);
    assert_eq!(interrupted["started"], json!([]));
    assert!(
        interrupted["refused"][&project_refusal]
            .as_str()
            .unwrap()
            .contains("Provider registry"),
        "{interrupted}"
    );
    let bound_before_admission = read(&binding_path(second_batch));
    assert_eq!(audit(), frozen);
    assert_eq!(fs::read(&provider_calls).unwrap(), first_calls);
    assert!(!fake.join("requests").exists());
    fs::write(&registry, registry_bytes).unwrap();

    mode("wrong-scope");
    fs::remove_file(&read_token).unwrap();
    let trace = fs::read(&reads).unwrap();
    fs::write(fake.join("expect-green"), "").unwrap();
    let second = tick(day_two);
    let second_id = second["started"][0]
        .as_str()
        .unwrap_or_else(|| panic!("{second}"));
    let waiting = &second["runs"][second_id];
    assert_eq!(waiting["status"], "waiting", "{second}");
    let (second_pin, second_raw) = index(waiting);
    assert_eq!(second_pin, bound_before_admission);
    assert_eq!(second_pin["batch_id"], second_batch);
    assert_ne!(first_pin["observation_id"], second_pin["observation_id"]);
    assert_ne!(first_raw, second_raw);
    assert!(!fake.join("requests").exists());
    let attempts = &waiting["frames"]["root"]["steps"][2]["intents"];
    private_write(&publish_token, "fixture_token\n");
    fs::write(fake.join("mode"), "lost_reply").unwrap();
    let lost = tick(day_two + 1);
    assert_eq!(lost["runs"][second_id]["status"], "waiting", "{lost}");
    assert!(fake.join("issue.json").exists());
    // The pending publication reconciles the original intent with the observer still unavailable.
    fs::write(fake.join("mode"), "normal").unwrap();
    let retry = tick(day_two + 2);
    assert_eq!(retry["started"], json!([]));
    let done = &retry["runs"][second_id];
    assert_eq!(done["status"], "succeeded", "{retry}");
    assert_eq!(
        done["frames"]["root"]["steps"][2]["intents"][0]["id"],
        attempts[0]["id"]
    );
    assert_eq!(index(done), (second_pin, second_raw));
    assert_eq!(index(first_run), (first_pin, first_raw));
    assert_eq!(fs::read(&reads).unwrap(), trace);
    let requests = http_requests(&fake);
    let posts: Vec<_> = requests
        .iter()
        .filter(|call| call["method"] == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0]["body"]["title"],
        "Green excludes its required boundary."
    );
    assert_eq!(tick(day_two + 3)["started"], json!([]));
    assert_eq!(http_requests(&fake), requests);
}

#[test]
fn portable_backlog_github_index_prevents_a_duplicate_from_the_daily_ant() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        507,
        BACKLOG,
    );
    let executable = github_fixture(&receiver);
    let reference = capture_backlog_batch(&receiver, Some(&executable), true);
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store = Path::new(project["store"].as_str().unwrap());
    let destination = read(&store.join("backlog-batch-policy-v2.json"))["destination_id"].clone();
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&receiver.repo)
        .output()
        .unwrap();
    assert!(head.status.success());
    let source = std::str::from_utf8(&head.stdout).unwrap();
    let observation = Command::new("python3")
        .args([
            "-I",
            "scripts/test-backlog-open-issues.py",
            "--scenario-observation",
        ])
        .current_dir(workspace())
        .output()
        .unwrap();
    assert!(observation.status.success(), "{:?}", observation.stderr);
    let observation = observation.stdout;
    let observation_id = af_store::canonical::blob_content_id(&observation);
    let observation_path = |id: &str| store.join(format!("backlog-open-issues-{}.json", &id[7..]));
    let path = observation_path(&observation_id);
    let pin_path = store.join("backlog-provider-index-v1.json");
    let pin = json!({
        "schema":"af/backlog-provider-index-pin@1",
        "project_id": reference["project_id"],
        "batch_id": reference["batch_id"],
        "source_revision": source.trim(),
        "destination_id": destination,
        "observation_id": observation_id,
    });
    schemas::valid(
        &schemas::validator("backlog-provider-index-pin-v1.json"),
        &pin,
    );
    let install_pin = |pin: &Value| {
        if pin_path.exists() {
            fs::remove_file(&pin_path).unwrap();
        }
        private_write(&pin_path, serde_json::to_vec(pin).unwrap());
    };
    install_pin(&pin);
    save(&receiver.input, &reference);
    let refuse = |diagnostic: &str| {
        let refusal = receiver.prepare_artifact(BACKLOG, None);
        assert!(!refusal.status.success());
        assert!(text(&refusal).contains(diagnostic), "{refusal:?}");
    };
    refuse("observation is unavailable");
    private_write(&path, &observation);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    refuse("observation is unavailable");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut replay = pin.clone();
    replay["batch_id"] = json!(format!("sha256:{}", "0".repeat(64)));
    install_pin(&replay);
    refuse("differs from the Project, batch, source or destination");
    let mut altered: Value = serde_json::from_slice(&observation).unwrap();
    for (field, value, diagnostic) in [
        (
            "repository_id",
            json!(24),
            "belongs to a different GitHub repository",
        ),
        (
            "observation",
            json!("incomplete"),
            "Invalid backlog provider observation",
        ),
    ] {
        let original = std::mem::replace(&mut altered[field], value);
        let bytes = serde_json::to_vec(&altered).unwrap();
        let id = af_store::canonical::blob_content_id(&bytes);
        private_write(&observation_path(&id), bytes);
        let mut wrong = pin.clone();
        wrong["observation_id"] = json!(id);
        install_pin(&wrong);
        refuse(diagnostic);
        altered[field] = original;
    }
    install_pin(&pin);
    fs::write(&path, b"{}").unwrap();
    refuse("observation differs from pinned bytes");
    fs::write(&path, &observation).unwrap();

    let prepared = ok(receiver.prepare_artifact(BACKLOG, None));
    let frozen = &prepared["definitions"]["daily-backlog"]["steps"][0]["operation"]["template"]["document"]
        ["requirements"]["scenario_portable_sources"]["sources"]["backlog-batch"]["text"];
    let frozen: Value = serde_json::from_str(frozen.as_str().unwrap()).unwrap();
    schemas::valid(
        &schemas::validator("backlog-collected-batch-v3.json"),
        &frozen,
    );
    assert_eq!(frozen["provider_index"]["observation_id"], observation_id);

    let fake = receiver.root.join("fake-github");
    let (run_id, first) = start_backlog_ant(&receiver, "indexed-backlog", &reference);
    assert_eq!(first["status"], "waiting", "{}", first["failure"]);
    assert!(!fake.join("requests").exists());
    let outcome = |id| {
        ok(receiver.af(&["backlog", "show", id, "--private", "--json"]))["data"]["outcomes"][0]
            .clone()
    };
    let tracked = outcome("blue-a");
    assert_eq!(tracked["disposition"], "already_tracked");
    assert_eq!(tracked, outcome("blue-b"));
    let reason = tracked["reason"].as_str().unwrap();
    assert!(reason.contains(&observation_id));
    assert!(reason.contains("GitHub repository ID 23"));
    assert!(reason.contains("Verified GitHub issue ID 1073 (#73)"));
    assert_eq!(outcome("unknown")["disposition"], "needs_evidence");
    assert_eq!(outcome("green")["disposition"], "verified");

    // A settled Run replays its accepted Task and decision, even if the observer's
    // files are rotated. The publication adapter may create only the distinct issue.
    fs::remove_file(&path).unwrap();
    install_pin(&replay);
    private_write(&store.join(GITHUB_ADAPTER), &executable);
    private_write(&store.join("backlog-github-token"), "fixture_token\n");
    fs::write(fake.join("expect-green"), "").unwrap();
    fs::write(fake.join("respond"), "").unwrap();
    let resume = [
        "scenario",
        "resume",
        &run_id,
        "--state",
        receiver.scenarios.to_str().unwrap(),
        "--json",
    ];
    for _ in 0..2 {
        let run = ok(receiver.af(&resume));
        assert_eq!(run["status"], "succeeded", "{}", run["failure"]);
    }
    assert_eq!(outcome("blue-a"), tracked);
    let requests = http_requests(&fake);
    let posts: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "POST")
        .collect();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0]["body"]["title"],
        "Green excludes its required boundary."
    );
    assert!(!posts[0].to_string().contains("Private index detail."));
    eprintln!(
        "Ant {run_id}, frozen observation {observation_id}, issue 1073 (#73) already tracked, one distinct POST"
    );
}

#[track_caller]
fn assert_backlog_publication_conformance(receiver: &Receiver, store: &Path, receipt: &Value) {
    let item_id = receipt["item_id"].as_str().unwrap();
    let public = ok(receiver.af(&["backlog", "show", item_id, "--json"]));
    assert_eq!(public["data"]["revision_id"], receipt["revision_id"]);
    assert!(public["data"]["public"]["title"].is_string());
    assert!(!public.to_string().contains("private-attachment"));
    assert!(public["data"]["evidence_ids"].is_null());

    let report = |id| ok(receiver.af(&["backlog", "show", id, "--private", "--json"]));
    let first = report("blue-a");
    let second = report("blue-b");
    let outcome = &first["data"]["outcomes"][0];
    assert_eq!(outcome, &second["data"]["outcomes"][0]);
    assert_eq!(outcome["disposition"], "verified");
    assert_eq!(outcome["decision_id"], receipt["decision_id"]);
    assert_eq!(outcome["item_id"], receipt["item_id"]);
    assert_eq!(outcome["revision_id"], receipt["revision_id"]);
    assert_eq!(
        report("unknown")["data"]["outcomes"][0]["disposition"],
        "needs_evidence"
    );

    let events = af_store::EventStore::open_read_only(store.join("events.sqlite"))
        .unwrap()
        .replay("backlog-processing")
        .unwrap();
    let decision = events
        .iter()
        .position(|event| {
            event.payload["schema"] == "af/BacklogDecision@2"
                && af_store::canonical::content_id(&event.payload).unwrap()
                    == receipt["decision_id"]
        })
        .unwrap();
    assert!(events[decision].payload["independent_verification_id"].is_string());
    let history: Vec<_> = events[decision + 1..]
        .iter()
        .filter(|event| event.payload["intent_id"] == receipt["intent_id"])
        .map(|event| &event.payload)
        .collect();
    assert!(history.len() >= 2);
    assert!(history[0]["receipt_id"].is_null());
    assert_eq!(history.last().unwrap(), &receipt);
    assert!(receipt["remote_id"].is_string());
    assert!(receipt["receipt_id"].is_string());
    assert!(!receipt["attempt_ids"].as_array().unwrap().is_empty());
    let policy = read(&store.join("backlog-batch-policy-v2.json"));
    assert_eq!(receipt["destination_id"], policy["destination_id"]);
}

fn configured_backlog_state(store: &Path) -> (Value, Value) {
    let events = af_store::EventStore::open_read_only(store.join("events.sqlite"))
        .unwrap()
        .replay("backlog-processing")
        .unwrap();
    let publication = events.last().unwrap().payload.clone();
    let state = Cas::open_existing(store.join("cas"))
        .unwrap()
        .get_json(publication["provider_state_id"].as_str().unwrap())
        .unwrap();
    schemas::valid(
        &schemas::validator("backlog-configured-state-v2.json"),
        &state,
    );
    (publication, state)
}

#[test]
fn portable_backlog_jira_reconciles_one_create_with_stable_id_and_human_edits() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        509,
        BACKLOG,
    );
    configure_backlog_receiver_fault_bounds(&receiver);
    let fake = receiver.root.join("fake-jira");
    fs::create_dir(&fake).unwrap();
    let script = include_str!("../../../../../scripts/project-backlog-jira.py");
    let fixture = include_str!("../../../../../fixtures/project-backlog/fake-jira.py").replace(
        "__AF_BACKLOG_FIXTURE_STATE__",
        &serde_json::to_string(&fake).unwrap(),
    );
    let (implementation, entrypoint) = script.split_once("\nif __name__ == ").unwrap();
    let executable = format!("{implementation}\n{fixture}\nif __name__ == {entrypoint}");
    let reference = capture_backlog_intake(
        &receiver,
        Some(BacklogFixture::Jira(&executable)),
        true,
        false,
    );
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store = Path::new(project["store"].as_str().unwrap());
    private_write(&store.join("backlog-jira-adapter"), executable);
    // Check the receiver refuses an unpinned executable before reserving a transport attempt.
    let adapter = store.join("backlog-jira-adapter");
    let original = fs::read(&adapter).unwrap();
    fs::write(&adapter, "changed").unwrap();
    let (run, first) = start_backlog_ant(&receiver, "jira-backlog", &reference);
    assert_eq!(first["status"], "waiting", "{}", first["failure"]);
    assert!(!fake.join("requests").exists());
    assert!(
        configured_backlog_state(store).0["attempt_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::write(&adapter, original).unwrap();
    private_write(
        &store.join("backlog-jira-credential"),
        r#"{"email":"fixture@example.invalid","api_token":"fixture_token"}"#,
    );
    let resume = || {
        ok(receiver.af(&[
            "scenario",
            "resume",
            &run,
            "--state",
            receiver.scenarios.to_str().unwrap(),
            "--json",
        ]))
    };
    assert_eq!(resume()["status"], "waiting");
    assert!(fake.join("issue.json").is_file());
    for (mode, reason) in [
        ("delayed", "not_observed"),
        ("wrong-type", "identity_conflict"),
        ("wrong-publisher", "identity_conflict"),
    ] {
        fs::write(fake.join("mode"), mode).unwrap();
        assert_eq!(resume()["status"], "waiting");
        let (publication, state) = configured_backlog_state(store);
        assert!(publication["receipt_id"].is_null());
        assert_eq!(state["outcome"]["kind"], reason);
    }
    let mut human = read(&fake.join("issue.json"));
    human["key"] = json!("AF-8");
    human["fields"]["summary"] = json!("Human edit");
    human["fields"]["status"]["statusCategory"]["key"] = json!("done");
    save(&fake.join("issue.json"), &human);
    fs::remove_file(fake.join("mode")).unwrap();
    assert_eq!(resume()["status"], "succeeded");
    let (publication, state) = configured_backlog_state(store);
    assert_backlog_publication_conformance(&receiver, store, &publication);
    assert_eq!(publication["remote_id"], "jira:10042");
    assert!(publication["receipt_id"].is_string());
    assert_eq!(state["outcome"]["observation"]["issue_key"], "AF-8");
    assert_eq!(state["outcome"]["observation"]["closed"], true);
    assert_eq!(state["outcome"]["approved_fields_match"], false);
    assert_eq!(read(&fake.join("issue.json")), human);
    let requests = http_requests(&fake);
    let writes: Vec<_> = requests
        .iter()
        .filter(|call| call["path"] == "/issue")
        .collect();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0]["body"]["fields"]["description"]["type"], "doc");
    assert!(
        !requests
            .iter()
            .any(|call| call.to_string().contains("private-attachment"))
    );
}

#[test]
fn portable_backlog_markdown_recovers_two_publications_in_the_configured_file() {
    let directory = tempfile::tempdir().unwrap();
    let receiver = Receiver::for_artifact(
        directory.path().canonicalize().unwrap().join("receiver"),
        510,
        BACKLOG,
    );
    configure_backlog_receiver_fault_bounds(&receiver);
    let destination = receiver.root.join("destination");
    fs::create_dir(&destination).unwrap();
    let path = destination.join("Human backlog.md");
    let human = "# Team backlog\r\n\r\nKeep these notes.\r\n\r\n## Housekeeping\r\nExisting prose.";
    fs::write(&path, human).unwrap();
    let reference = capture_backlog_intake(
        &receiver,
        Some(BacklogFixture::Markdown(&path)),
        false,
        false,
    );
    let project = ok(receiver.af(&["project", "show", "--json"]));
    let store = Path::new(project["store"].as_str().unwrap());
    let config = read(&store.join("backlog-markdown-destination-v2.json"));
    let validator = schemas::validator("backlog-markdown-destination-v2.json");
    let mut repository_scope = config.clone();
    repository_scope["scope"] = json!("repository");
    assert!(!validator.is_valid(&repository_scope));
    let mut old = config.clone();
    old["schema"] = json!("af/backlog-markdown-destination@1");
    assert!(!validator.is_valid(&old));

    // Abort only receipt insertion after AF has actually synced the first append.
    // The already committed attempt remains the recovery authority.
    let database = rusqlite::Connection::open(store.join("events.sqlite")).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER lose_markdown_receipt BEFORE INSERT ON events
         WHEN NEW.run_id = 'backlog-processing'
           AND json_extract(NEW.payload, '$.receipt_id') IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'fixture lost receipt'); END;",
        )
        .unwrap();
    let editor = fs::OpenOptions::new().write(true).open(&path).unwrap();
    rustix::fs::flock(
        &editor,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    let (run, outcome) = start_backlog_ant(&receiver, "markdown-backlog", &reference);
    assert_eq!(outcome["status"], "waiting", "{}", outcome["failure"]);
    assert_eq!(
        configured_backlog_state(store).1["outcome"]["kind"],
        "prewrite_conflict"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), human);
    drop(editor);
    let resume = || {
        ok(receiver.af(&[
            "scenario",
            "resume",
            &run,
            "--state",
            receiver.scenarios.to_str().unwrap(),
            "--json",
        ]))
    };
    // The grant pins the path, and acquiring Git metadata at that path cannot widen it.
    git(&destination, &["init", "-q"]);
    git(&destination, &["add", "--", "Human backlog.md"]);
    assert_eq!(resume()["status"], "waiting");
    assert_eq!(
        configured_backlog_state(store).1["outcome"]["kind"],
        "prewrite_conflict"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), human);
    fs::rename(destination.join(".git"), receiver.root.join("disabled-git")).unwrap();

    assert_eq!(resume()["status"], "waiting");
    let (first, state) = configured_backlog_state(store);
    assert!(first["receipt_id"].is_null());
    assert_eq!(state["outcome"]["kind"], "attempt_prepared");
    let original = fs::read_to_string(&path).unwrap();
    let first_entry = original.strip_prefix(human).unwrap();
    let blue = "Blue excludes its required boundary.";
    let green = "Green excludes its required boundary.";
    let (first_title, second_title) = if first_entry.contains(blue) {
        (blue, green)
    } else {
        (green, blue)
    };
    assert!(first_entry.contains(first_title));
    assert_eq!(first_entry.matches("<!-- af-backlog-entry-v1:").count(), 1);
    assert!(first_entry.contains(first["intent_id"].as_str().unwrap()));
    let expected_grant = format!(
        "markdown-backlog:{}:{}",
        reference["project_id"].as_str().unwrap(),
        af_store::canonical::content_id(&config).unwrap()
    );
    let prepared = read(&receiver.root.join("ant.json"));
    let grants = prepared["scenario"]["definitions"]["daily-backlog"]["authority"]["grants"]
        .as_array()
        .unwrap();
    assert_eq!(grants.len(), 2);
    assert!(
        grants
            .iter()
            .all(|grant| grant["destination"] == expected_grant)
    );
    assert!(!prepared.to_string().contains(path.to_str().unwrap()));

    database
        .execute_batch("DROP TRIGGER lose_markdown_receipt;")
        .unwrap();
    let human_change = "\n\nOperator added a note between deliveries.\n";
    let edited = format!("{human}{human_change}{first_entry}");
    for conflict in [
        edited.replace(
            "<!-- af-backlog-entry-v1:",
            "<!-- af-backlog-entry-v1-edited:",
        ),
        edited.replace(first_title, "A human edited the managed heading."),
        format!("{edited}{first_entry}"),
    ] {
        fs::write(&path, &conflict).unwrap();
        assert_eq!(resume()["status"], "waiting");
        let (_, state) = configured_backlog_state(store);
        assert_eq!(state["outcome"]["kind"], "identity_conflict");
        assert_eq!(fs::read_to_string(&path).unwrap(), conflict);
    }

    fs::write(&path, &edited).unwrap();
    let finished = resume();
    assert_eq!(finished["status"], "succeeded", "{}", finished["failure"]);
    assert_eq!(
        finished["frames"]["root"]["outputs"]["assessment"]["value"]["result"]["acceptance"],
        "satisfied"
    );
    let document = fs::read_to_string(&path).unwrap();
    let second_entry = document.strip_prefix(&edited).unwrap();
    assert!(second_entry.contains(second_title));
    assert_eq!(document.matches("<!-- af-backlog-entry-v1:").count(), 2);
    assert_eq!(
        document
            .matches("The configured boundary must be inclusive.")
            .count(),
        2
    );
    for private in [
        "private-attachment",
        "mismatched-attachment",
        "private-insufficient-repro",
        "An unrelated claim",
        "The reported behavior is unknown.",
    ] {
        assert!(!document.contains(private));
    }
    let (_, state) = configured_backlog_state(store);
    assert_eq!(state["outcome"]["approved_fields_match"], true);
    let receipts: Vec<_> = af_store::EventStore::open_read_only(store.join("events.sqlite"))
        .unwrap()
        .replay("backlog-processing")
        .unwrap()
        .into_iter()
        .map(|event| event.payload)
        .filter(|payload| payload["receipt_id"].is_string())
        .collect();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0]["intent_id"], first["intent_id"]);
    assert_ne!(receipts[0]["remote_id"], receipts[1]["remote_id"]);
    let blue_receipt = &receipts[usize::from(first_title != blue)];
    assert_backlog_publication_conformance(&receiver, store, blue_receipt);
    assert_eq!(resume(), finished);
    assert_eq!(fs::read_to_string(&path).unwrap(), document);
}
