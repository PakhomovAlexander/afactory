//! The shipped production loop, with real model bindings and deterministic fake Provider IO.
use super::*;
use af_store::Cas;
use std::os::unix::fs::MetadataExt;

fn executable_digest(path: &Path) -> String {
    use std::io::Read;
    let mut source = std::fs::File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = source.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("sha256:{}", af_contracts::hex::encode(&digest.finalize()))
}

// Installed native readers are bounded at 256 MiB. Linux debug builds can exceed
// that bound: remove only debug symbols from an owned copy, never the held binary
// (which shares its inode with other tests). `strip -S` removes debug symbols on
// both ELF (GNU strip) and Mach-O (Apple strip), preserving runtime symbols.
fn installed_native_reader(root: &Path) -> (PathBuf, String) {
    let source = crate::common::af_binary();
    let original = std::fs::metadata(source).unwrap();
    let original_digest = executable_digest(source);
    let directory = root.join("installed-native-reader");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let binary = directory.join("af");
    let mut copy = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&binary)
        .unwrap();
    std::io::copy(&mut std::fs::File::open(source).unwrap(), &mut copy).unwrap();
    drop(copy);
    let copied = std::fs::metadata(&binary).unwrap();
    assert_ne!(
        (original.dev(), original.ino()),
        (copied.dev(), copied.ino())
    );
    assert_eq!(executable_digest(&binary), original_digest);
    let stripped = std::process::Command::new("/usr/bin/strip")
        .arg("-S")
        .arg(&binary)
        .output()
        .unwrap();
    assert_eq!(executable_digest(source), original_digest);
    let unchanged = std::fs::metadata(source).unwrap();
    assert_eq!(
        unchanged.permissions().mode(),
        original.permissions().mode()
    );
    assert_eq!(
        (unchanged.dev(), unchanged.ino()),
        (original.dev(), original.ino())
    );
    assert!(
        stripped.status.success(),
        "{}",
        String::from_utf8_lossy(&stripped.stderr)
    );
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o500)).unwrap();
    let installed = std::fs::metadata(&binary).unwrap();
    assert!(installed.len() <= 256 * 1024 * 1024);
    assert_eq!(installed.permissions().mode() & 0o777, 0o500);
    assert_eq!(
        std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let digest = executable_digest(&binary);
    (binary, digest)
}

fn copy_mutable_fixture_file(source: &Path, target: &Path) {
    std::fs::copy(source, target).unwrap();
    let mut permissions = std::fs::metadata(target).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    std::fs::set_permissions(target, permissions).unwrap();
}

// Explicit generic contract authority is copied into a disposable consuming repository.
// Sealed gate inputs stay read-only; only the copies gain their owner-write bit.
fn copy_operator_authority(source: &Path, repo: &Path) {
    for name in ["task-catalog.toml", "report-policy.toml"] {
        let target = repo.join(".af").join(name);
        copy_mutable_fixture_file(&source.join(name), &target);
    }
}

#[test]
fn copied_operator_authority_is_writable_under_a_read_only_gate() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sealed");
    let repo = root.path().join("consumer");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir_all(repo.join(".af")).unwrap();
    for (name, mode) in [("task-catalog.toml", 0o444), ("report-policy.toml", 0o400)] {
        let path = source.join(name);
        std::fs::write(&path, "sealed authority\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    copy_operator_authority(&source, &repo);
    for (name, mode) in [("task-catalog.toml", 0o444), ("report-policy.toml", 0o400)] {
        let target = repo.join(".af").join(name);
        assert_eq!(std::fs::read(&target).unwrap(), b"sealed authority\n");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            mode | 0o200
        );
        std::fs::write(target, "consumer authority\n").unwrap();
        let original = source.join(name);
        assert_eq!(std::fs::read(&original).unwrap(), b"sealed authority\n");
        assert_eq!(
            std::fs::metadata(original).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}

struct Performance {
    f: Fixture,
    task_state: PathBuf,
    home: PathBuf,
    path: std::ffi::OsString,
    commit: String,
    packet_source: PathBuf,
}
impl Performance {
    fn new() -> Self {
        Self::from_source(&workspace().join("fixtures/native-catalog-contracts"))
    }
    fn from_source(source: &Path) -> Self {
        let f = Fixture::bare();
        std::fs::create_dir(f.repo.join(".af")).unwrap();
        copy_operator_authority(&source.join(".af"), &f.repo);
        // This fixture is a tiny consuming project. Its local Gate checks its own file;
        // production package reservations and all independent judging slots are unchanged.
        std::fs::write(f.repo.join("subject.txt"), "bounded performance subject\n").unwrap();
        let check = |name: &str| json!({"name":name,"required":true,"command":{"program":"/usr/bin/python3","args":[{"value":"-c","provenance":"literal"},{"value":"assert open('subject.txt').read() == 'bounded performance subject\\n'; assert open('optimized.txt').read() == 'cache decoded manifest by exact digest\\n'","provenance":"literal"}]}});
        std::fs::write(f.repo.join(".af/code-policy.toml"), toml::to_string(&json!({"schema":"af.code-task-policy/1","check_wall_ms":5000,"require_container":false,"checks":{"kernel":check("kernel"),"markdownlint":check("markdownlint")}})).unwrap()).unwrap();
        let catalog_path = f.repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let needed = [
            "kernel/implementation-reviewed",
            "kernel/review-code",
            "kernel/verification-reviewed",
            "kernel/report",
            "kernel/implementer",
            "kernel/bugs",
            "kernel/correctness",
            "kernel/evaluator",
            "kernel/analyst",
            "kernel/report-verifier",
            "warm/verification-kind",
            "owner-default/implementer",
            "owner-default/bugs",
            "owner-default/correctness",
            "owner-default/evaluator",
            "owner-default/report-author",
        ];
        catalog["packages"]
            .as_table_mut()
            .unwrap()
            .retain(|name, _| needed.contains(&name));
        catalog["providers"]
            .as_table_mut()
            .unwrap()
            .retain(|name, _| needed.contains(&name));
        // Kind authority must close over the same captured package subset.
        catalog["kinds"]
            .as_table_mut()
            .unwrap()
            .retain(|_, package| needed.contains(&package.as_str().unwrap()));
        for pin in catalog["packages"].as_table().unwrap().values() {
            let path = pin["path"].as_str().unwrap();
            crate::task_cli::copy_tree(&source.join(path), &f.repo.join(path));
        }
        // Keep the shipped manifests and catalog digests exact. Role/account selection is
        // supplied only by the same slot/provider bindings the operator captures.
        std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let fixture: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let shipped: toml::Value =
            toml::from_str(&std::fs::read_to_string(source.join(".af/task-catalog.toml")).unwrap())
                .unwrap();
        let committed: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                workspace().join("fixtures/native-catalog-contracts/.af/task-catalog.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fixture["review"], shipped["review"]);
        assert_eq!(fixture["review"], committed["review"]);
        crate::task_report::git(&f.repo, &["init", "-q", "-b", "main"]);
        crate::task_report::git(&f.repo, &["add", "-A"]);
        crate::task_report::git(&f.repo, &["commit", "-qm", "pinned owner authority"]);
        let out = std::process::Command::new("git")
            .current_dir(&f.repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let commit = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        let home = f.machine.join("provider");
        let bin = f.machine.join("bin");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&bin).unwrap();
        let native = r#"#!/usr/bin/python3 -B
import json,os,sys,pathlib
home=pathlib.Path(os.environ['CODEX_HOME'])
if sys.argv[1:]==['--version']:
 print('codex-cli 0.121.0'); sys.exit(0)
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  r=json.loads(line)
  if r.get('id')==1: print(json.dumps({'id':1,'result':{}}),flush=True)
  if r.get('id')==2: print(json.dumps({'id':2,'result':{'account':{'type':'chatgpt','email':'scenario@example.invalid'}}}),flush=True)
 sys.exit(0)
request=sys.stdin.read()
with (home/'calls.jsonl').open('a') as f: f.write(json.dumps({'probe':request==PROBE_INPUT})+'\n')
if request==PROBE_INPUT:
 message='OK'
else:
 r=json.loads(request); i=r['inputs']; ports=r['output_schemas']
 with (home/'requests.jsonl').open('a') as f: f.write(json.dumps(r)+'\n')
 if 'assignment' in i:
  reply={'result':[{'reports':[],'benchmark_demands':[],'dispositions':[]}]}
 elif 'document' in i:
  reply={'result':[dict(document_id=i['document'][0]['artifact_id'], sources_id=i['document'][0]['payload']['sources_id'], requirements_id=i['requirements'][0]['artifact_id'], check_receipt_id=i['checks'][0]['artifact_id'], source_snapshot_id=i['source'][0]['snapshot_id'], outcome=('failed' if (home/'refuse-report').exists() else 'passed'), summary='independent report verification')]}
 elif 'draft' in ports:
  text='DISTINCT-RESEARCH: cache decoded manifest by exact digest; paired timing must improve.'
  if (home/'expect-native-findings').exists() and 'research_report' not in i:
   packet=json.loads(i['requirements'][0]['payload']['specification']['scenario_evidence']['evidence'])
   assert len(packet['items'])==2
   assert 'Decode the exact digest once' in json.dumps(packet)
   assert 'Nothing says what offset and limit mean' in json.dumps(packet)
   text+=' Prior native finding artifact '+packet['items'][0]['artifact']['native_receipt']['artifact_id']
  if (home/'oversized-research').exists(): text+='x'*40000
  if (home/'no-worthwhile').exists(): text='NO-WORTHWHILE: bounded research found no worthwhile candidate; costs and uncertainty dominate the retained measurements.'
  if 'research_report' in i:
   assert 'DISTINCT-RESEARCH' in i['research_report'][0]['payload']['text']
   text+=' Final report retains research artifact '+i['research_report'][0]['artifact_id']+' and its evidence: '+i['research_report'][0]['payload']['text']
  reply={'draft':[{'schema':'af.document-draft/3','title':'Research chain','sections':[{'heading':'Findings','body':text},{'heading':'Recommendation','body':text}],'citations':[{'kind':'repository','location':{'path':'subject.txt','line':1}}]}]}
 elif 'report' in ports:
  if not (home/'scoped-mode').exists(): assert 'DISTINCT-RESEARCH' in i['research_report'][0]['payload']['text']
  root=pathlib.Path(sys.argv[sys.argv.index('-C')+1]) if '-C' in sys.argv else pathlib.Path.cwd()
  if (home/'scoped-mode').exists() and (root/'optimized.txt').exists(): (root/'repair.txt').write_text('independently accepted same-PR repair\n')
  mode=(home/'scoped-mode').read_text() if (home/'scoped-mode').exists() else ''
  if mode in ('gate-review','verifier'):
   stage=int((root/'lifecycle-stage').read_text())+1 if (root/'lifecycle-stage').exists() else 0
   if stage:
    evidence=i['requirements'][0]['payload']['specification']['scenario_native_outcome']
    findings=i['findings'][0]['payload']
    assert findings['continues']['result_id']==evidence['receipt']['result_id']
    assert findings['continues']['snapshot_id']==evidence['repair']['snapshot_id']
    (root/'seen-findings.json').write_text(json.dumps(findings))
   (root/'lifecycle-stage').write_text(str(stage))
  (root/'optimized.txt').write_text('wrong initial gate output\n' if mode=='gate-review' and stage==0 else 'cache decoded manifest by exact digest\n')
  reply={'report':[{'summary':'Constructed the exact accepted research recommendation.'}]}
 else:
  outcome='passed'
  mode=(home/'scoped-mode').read_text() if (home/'scoped-mode').exists() else ''
  if mode in ('gate-review','verifier'):
   root=pathlib.Path(sys.argv[sys.argv.index('-C')+1]) if '-C' in sys.argv else pathlib.Path.cwd()
   stage=int((root/'lifecycle-stage').read_text())
   counter=home/'evaluation-count'
   count=int(counter.read_text())+1 if counter.exists() else 1
   counter.write_text(str(count))
   if mode=='gate-review' and stage==1 or mode=='verifier' and count==2:
    outcome='failed'
  reply={'result':[{'outcome':outcome,'reason':'native independent evaluator finding; repair the whole original goal' if outcome=='failed' else 'exact source independently checked'}]}
 message=json.dumps({'schema':'af.worker-reply/1','outputs':reply})
if '-o' in sys.argv: pathlib.Path(sys.argv[sys.argv.index('-o')+1]).write_text(message)
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':8000001 if (home/'scoped-mode').exists() and (home/'scoped-mode').read_text()=='budget' and request!=PROBE_INPUT else 3,'cached_input_tokens':0,'output_tokens':2}}))
"#;
        std::fs::write(
            bin.join("codex"),
            crate::provider_cli::with_help("codex", &crate::provider_cli::with_probe(native)),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("codex"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(home.join("providers.toml"), toml::to_string(&json!({"version":4,"providers":[{"id":"codex-personal","kind":"codex","auth_dir":home}]})).unwrap()).unwrap();
        let path = std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
            .unwrap();
        let task_state = f.machine.join("tasks");
        Self {
            f,
            home,
            path,
            task_state,
            commit,
            packet_source: workspace()
                .join("fixtures/performance/schema/evidence-packet/valid.json"),
        }
    }
    fn output(&self, state: &Path, args: &[&str]) -> Output {
        crate::common::af()
            .current_dir(&self.f.repo)
            .env("HOME", &self.home)
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", self.home.join("providers.toml"))
            .args(args)
            .args(["--state", state.to_str().unwrap(), "--json"])
            .output()
            .unwrap()
    }
    fn cli(&self, state: &Path, args: &[&str]) -> Value {
        value(self.output(state, args), 0)
    }
    fn bindings(&self, kind: &str) -> PathBuf {
        let name = match kind {
            "implement" => "implementation",
            "verification" => "verification",
            "report" => "report",
            _ => panic!("unexpected production stage {kind}"),
        };
        workspace().join(format!(
            "fixtures/scenarios/performance-{name}-bindings.json"
        ))
    }
    fn bundle(&self) -> Value {
        self.bundle_from("performance-candidate.json")
    }
    fn bundle_from(&self, name: &str) -> Value {
        let mut bundle: Value = serde_json::from_slice(
            &std::fs::read(workspace().join("fixtures/scenarios").join(name)).unwrap(),
        )
        .unwrap();
        let definition = &mut bundle["definitions"]["performance"];
        let empty_packet: Value = serde_json::from_slice(
            &std::fs::read(
                workspace().join("fixtures/performance/schema/evidence-packet/valid.json"),
            )
            .unwrap(),
        )
        .unwrap();
        definition["steps"][0]["inputs"]["subject"]["value"]["value"] =
            empty_packet["index_digest"].clone();
        definition["steps"][0]["inputs"]["revision"]["value"]["value"] =
            empty_packet["compatibility"].clone();
        let mut pins = vec![];
        for step in definition["steps"].as_array_mut().unwrap() {
            if step["operation"]["kind"] != "fresh_task" {
                continue;
            }
            let file = self
                .f
                .machine
                .join(format!("{}-template.json", step["id"].as_str().unwrap()));
            write(&file, &step["operation"]["template"]["document"]);
            let bindings = self.bindings(
                step["operation"]["template"]["document"]["kind"]
                    .as_str()
                    .unwrap(),
            );
            let pinned = self.cli(
                &self.f.state,
                &[
                    "scenario",
                    "template-pin",
                    "--file",
                    file.to_str().unwrap(),
                    "--repo",
                    self.f.repo.to_str().unwrap(),
                    "--task-state",
                    self.task_state.to_str().unwrap(),
                    "--authority-commit",
                    &self.commit,
                    "--bindings",
                    bindings.to_str().unwrap(),
                ],
            );
            step["operation"]["template"] = pinned["template"].clone();
            pins.push(pinned["template_id"].clone());
        }
        definition["authority"]["task_templates"] = json!(pins);
        let mut policy: Value =
            serde_json::from_slice(&std::fs::read(&self.f.policy).unwrap()).unwrap();
        policy["task_templates"] = definition["authority"]["task_templates"].clone();
        let adapter = self.f.machine.join("native-lab-fixture");
        let text = format!(
            r#"#!/usr/bin/python3 -B
import json,sys,pathlib
root=pathlib.Path({root:?})
r=json.load(sys.stdin); i=r['inputs']; method=r['method']
o=dict(schema='af/scenario-adapter-response@3',intent_id=r['intent_id'],input_digest=r['input_digest'],outcome='done',evidence='native fixture receipt',observation=None)
if method=='observe':
 condition=i['condition']['value']
 ready=True
 if condition=='bounded_evidence':
  evidence=(root/'research-packet.json').read_text()
  packet=json.loads(evidence); assert packet['index_digest']==i['subject']['value']; assert packet['compatibility']==i['revision']['value']
 else:
  completed=root/'completion.json'; ready=completed.exists()
  evidence=completed.read_text() if ready else 'native report is not yet verified'
  if ready:
   c=json.loads(evidence); submitted=json.load((root/'submission.json').open())
   assert submitted['intent_id']==i['subject']['value']
   assert c['candidate_snapshot_id']==i['revision']['value']
 o['outcome']='done' if ready else 'pending'
 o['observation']=dict(subject=i['subject']['value'],revision=i['revision']['value'],condition=condition,satisfied=ready,evidence=evidence)
else:
 p=root/'submission.json'
 if p.exists():
  assert json.load(p.open())['intent_id']==r['intent_id']
 elif method=='reconcile': o['outcome']='absent'
 else:
  json.dump(r,p.open('w'))
  sys.exit(2) # response lost after the only durable effect
print(json.dumps(o))
"#,
            root = self.f.machine.display().to_string()
        );
        // This packet is replaced by native prior findings later in the complete loop.
        copy_mutable_fixture_file(
            &self.packet_source,
            &self.f.machine.join("research-packet.json"),
        );
        std::fs::write(&adapter, &text).unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        policy["adapters"] = json!({"lab":{"executable":adapter,"digest":format!("sha256:{}", af_contracts::hex::encode(&Sha256::digest(text.as_bytes()))),"operations":["observe","experiment_submit_v1"],"destinations":["native:claw"],"max_wall_ms":30000,"max_output_bytes":65536}});
        write(&self.f.policy, &policy);
        let mut proposal: Value = serde_json::from_slice(
            &std::fs::read(workspace().join("fixtures/scenarios/performance-native-roadmap.json"))
                .unwrap(),
        )
        .unwrap();
        let now = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        proposal["horizon"] = json!({"start_unix_ms":now,"end_unix_ms":now+691200000});
        let file = self.f.machine.join("roadmap.json");
        write(&file, &proposal);
        let revision = self.cli(
            &self.f.state,
            &[
                "scenario",
                "roadmap",
                "validate",
                "--file",
                file.to_str().unwrap(),
            ],
        );
        let roadmap = self.cli(
            &self.f.state,
            &[
                "scenario",
                "roadmap",
                "create",
                "--file",
                file.to_str().unwrap(),
                "--confirm-revision",
                revision["revision"].as_str().unwrap(),
            ],
        );
        definition["admission"] = json!({"roadmap":proposal["id"],"identity":roadmap["identity"],"revision":revision["revision"]});
        bundle
    }
    fn start(&self, label: &str, bundle: &Value) -> Value {
        self.start_recorded(label, bundle, true)
    }
    fn start_recorded(&self, label: &str, bundle: &Value, execute: bool) -> Value {
        write(&self.f.bundle, bundle);
        let pin = self.cli(
            &self.f.state,
            &[
                "scenario",
                "validate",
                "--file",
                self.f.bundle.to_str().unwrap(),
                "--authority",
                self.f.policy.to_str().unwrap(),
            ],
        );
        self.cli(
            &self.f.state,
            &[
                "scenario",
                "start",
                label,
                "--file",
                self.f.bundle.to_str().unwrap(),
                "--authority",
                self.f.policy.to_str().unwrap(),
                "--inputs",
                self.f.inputs.to_str().unwrap(),
                "--confirm-pin",
                pin["revision"].as_str().unwrap(),
                if execute {
                    "--execute"
                } else {
                    "--require-startup-scope"
                },
            ],
        )
    }
}

/// These are instruction/contract assertions, not fake Provider compliance claims. The
/// captured production template has a goal and scenario_mode, with no campaign package plan.
fn assert_owner_performance_instruction_contracts(repo: &Path, candidate: &Value) {
    assert_eq!(candidate["kind"], "implement");
    assert_eq!(
        candidate["requirements"],
        json!({"scenario_mode": "candidate"})
    );
    let goal = candidate["goal"].as_str().unwrap();
    for obligation in [
        "code correctness and guardrails only",
        "Do not claim measured improvement here",
        "Consume the accepted research_report artifact",
        "retain its recommendation and provenance",
    ] {
        assert!(goal.contains(obligation), "{obligation}: {goal}");
    }
    for role in ["implementer", "bugs", "correctness", "evaluator"] {
        let prompt = std::fs::read_to_string(repo.join(format!(
            ".af/task-packages/owner-default/{role}/instructions.md"
        )))
        .unwrap();
        let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        for obligation in [
            "actual Task goal and requirements",
            "`scenario_goal`",
            "`scenario_evidence`",
            "only when the actual requirements or binding decisions require",
            "evidence, never execution or publication authority",
            "research_report",
            "code correctness and guardrails",
        ] {
            assert!(prompt.contains(obligation), "{role}: missing {obligation}");
        }
        for invented in [
            "one kernel campaign package",
            "requirements payload names the plan document",
            "plan's fixed requirements",
            "One ADR under",
            "ADR and changelog entries exist",
            "no edits to `.af/`",
        ] {
            assert!(!prompt.contains(invented), "{role}: invented {invented}");
        }
    }
}

/// Prompts name the evidence actually available to independent judges, not team results
/// that only native acceptance can gather. Compare both stages' real node bindings with
/// the shipped Worker signatures; diff_subject is optional kernel-derived context.
#[test]
fn owner_judge_evidence_matches_candidate_and_verification_input_contracts() {
    let repo = workspace().join("fixtures/native-catalog-contracts");
    let pipeline = |name: &str| -> toml::Value {
        toml::from_str(
            &std::fs::read_to_string(
                repo.join(format!(".af/task-packages/kernel/{name}/pipeline.toml")),
            )
            .unwrap(),
        )
        .unwrap()
    };
    let review = pipeline("review-code");
    for stage in ["implementation-reviewed", "verification-reviewed"] {
        let manifest = pipeline(stage);
        let nodes = manifest["nodes"].as_array().unwrap();
        let call = nodes
            .iter()
            .find(|n| n["id"].as_str() == Some("review"))
            .unwrap();
        assert_eq!(
            call["operator"]["pipeline"].as_str(),
            Some("kernel/review-code")
        );
        for role in ["bugs", "correctness", "evaluator"] {
            let package = repo.join(format!(".af/task-packages/owner-default/{role}"));
            let worker: toml::Value =
                toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap())
                    .unwrap();
            let prompt = std::fs::read_to_string(package.join("instructions.md")).unwrap();
            let declared: std::collections::BTreeSet<_> = prompt
                .lines()
                .find_map(|line| line.strip_prefix("Declared evidence inputs: "))
                .unwrap()
                .strip_suffix('.')
                .unwrap()
                .split(", ")
                .collect();
            let inputs = worker["signature"]["contract"]["inputs"]
                .as_table()
                .unwrap();
            assert_eq!(
                declared,
                inputs
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>(),
                "{stage}/{role}"
            );
            for port in inputs.values() {
                assert!(
                    ![
                        "review.kernel/ReviewerResult@2",
                        "af/TaskEvaluation@1",
                        "af/ImplementationReport@1"
                    ]
                    .contains(&port["artifact_type"].as_str().unwrap())
                );
            }
            assert_eq!(inputs["diff_subject"]["optional"].as_bool(), Some(true));
            let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
            for obligation in [
                "Judge only your declared delivered inputs",
                "Do not require your own future result or sibling judge results",
                "The native pipeline and publication acceptance enforce the complete selected bugs, correctness and evaluator team and its results",
                "Consume each delivered artifact on these ports; optional inputs apply only when present",
            ] {
                assert!(prompt.contains(obligation), "{stage}/{role}: {obligation}");
            }
            assert!(
                !prompt.contains("Require the selected bugs, correctness and evaluator results")
            );
            let node = if role == "evaluator" {
                nodes
                    .iter()
                    .find(|n| n["id"].as_str() == Some("evaluate_goal"))
                    .unwrap()
            } else {
                assert_eq!(call["operator"]["bindings"][role].as_str(), Some(role));
                review["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|n| n["id"].as_str() == Some(role))
                    .unwrap()
            };
            assert_eq!(node["operator"]["slot"].as_str(), Some(role));
            let bound = node["inputs"].as_table().unwrap();
            assert_eq!(
                bound
                    .keys()
                    .map(String::as_str)
                    .collect::<std::collections::BTreeSet<_>>(),
                declared
                    .into_iter()
                    .filter(|p| *p != "diff_subject")
                    .collect(),
                "{stage}/{role}"
            );
            let expected = if role == "evaluator" {
                json!({
                    "checks": {"kind":"node", "node":"final_checks", "port":"output"},
                    "requirements": {"kind":"input", "port":"requirements"},
                    "source": {"kind":"node", "node":"accept", "port":"snapshot"}
                })
            } else {
                json!({
                    "assignment": {"kind":"node", "node":"bind", "port":role},
                    "checks": {"kind":"node", "node":"checks", "port":"result"},
                    "history": {"kind":"input", "port":"history"},
                    "requirements": {"kind":"input", "port":"requirements"},
                    "source": {"kind":"input", "port":"source"},
                    "subject": {"kind":"node", "node":"bind", "port":"subject"}
                })
            };
            assert_eq!(
                serde_json::to_value(bound).unwrap(),
                expected,
                "{stage}/{role}"
            );
        }
    }
}

/// Exercise the shipped templates and captured catalog without Provider inference. Native
/// planning still checks CLI installation/account identity; the fake accepts only those
/// token-free checks and traps every other invocation. No package or budget is rewritten.
#[test]
fn committed_owner_catalog_plans_all_four_production_stages_without_provider_inference() {
    let p = Performance::new();
    let marker = p.home.join("unexpected-provider-inference");
    let trap = format!(
        r#"#!/usr/bin/python3 -B
import json,sys,pathlib
if sys.argv[1:]==['--version']:
 print('codex-cli 0.121.0'); sys.exit(0)
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  r=json.loads(line)
  if r.get('id')==1: print(json.dumps({{'id':1,'result':{{}}}}),flush=True)
  if r.get('id')==2: print(json.dumps({{'id':2,'result':{{'account':{{'type':'chatgpt','email':'scenario@example.invalid'}}}}}}),flush=True)
 sys.exit(0)
pathlib.Path({marker:?}).write_text('unexpected inference')
sys.exit(99)
"#,
        marker = marker.display().to_string()
    );
    let bin = p.f.machine.join("bin");
    for name in ["codex", "claude"] {
        let executable = bin.join(name);
        std::fs::write(&executable, crate::provider_cli::with_help(name, &trap)).unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let bundle = p.bundle();
    let mut stages = Vec::new();
    for step in bundle["definitions"]["performance"]["steps"]
        .as_array()
        .unwrap()
    {
        if step["operation"]["kind"] != "fresh_task" {
            continue;
        }
        let stage = step["id"].as_str().unwrap();
        stages.push(stage);
        let template = &step["operation"]["template"];
        assert_eq!(template["authority_commit"], p.commit);
        let bindings = &template["bindings"];
        let document = &template["document"];
        let kind = document["kind"].as_str().unwrap();
        let binding_file = p.bindings(kind);
        let mut expected_bindings: Value =
            serde_json::from_slice(&std::fs::read(&binding_file).unwrap()).unwrap();
        expected_bindings["packages"] = json!({});
        assert_eq!(bindings, &expected_bindings, "{stage}");
        if kind == "implement" {
            assert_owner_performance_instruction_contracts(&p.f.repo, document);
        }
        let file = p.f.machine.join(format!("{stage}-template.json"));
        let planned = p.cli(
            &p.task_state,
            &[
                "task",
                "plan",
                "--file",
                file.to_str().unwrap(),
                "--authority",
                &p.commit,
                "--bindings",
                binding_file.to_str().unwrap(),
            ],
        );
        assert_eq!(planned["attempts"], 0, "{planned:#}");
        assert_eq!(planned["chargeable_tokens"], "0");
        // Native planning consumes part of the requested wall before admission. As in
        // scenario_template::validate_recorded_plan, its recorded allowance can only shrink.
        assert!(
            planned["clock"]["wall_ms"].as_u64().unwrap()
                <= document["limits"]["wall_ms"].as_u64().unwrap()
        );
        let plan = &planned["plan"];
        for field in ["tokens", "max_attempts", "verification"] {
            assert_eq!(plan["limits"][field], document["limits"][field]);
        }
        assert_eq!(plan["generated_origins"], json!([]));
        let expected: Vec<_> = if kind == "report" {
            vec![
                (
                    "author",
                    "owner-default/report-author",
                    "gpt-6.2-sol",
                    "medium",
                ),
                (
                    "verifier",
                    "kernel/report-verifier",
                    "gpt-6.1-terra",
                    "high",
                ),
            ]
        } else {
            let mut roles = vec![
                ("bugs", "owner-default/bugs", "gpt-6.1-terra", "high"),
                (
                    "correctness",
                    "owner-default/correctness",
                    "gpt-6.1-terra",
                    "high",
                ),
                (
                    "evaluator",
                    "owner-default/evaluator",
                    "gpt-6.1-terra",
                    "medium",
                ),
            ];
            if kind == "implement" {
                roles.push((
                    "implementer",
                    "owner-default/implementer",
                    "gpt-6.2-sol",
                    "medium",
                ));
            }
            roles
        };
        assert_eq!(plan["bindings"].as_object().unwrap().len(), expected.len());
        for (role, worker, model, effort) in expected {
            let slot = format!("root.slots.{role}");
            assert_eq!(planned["graph"]["slots"][&slot]["worker"], worker);
            let execution = &plan["bindings"][&slot]["execution"];
            assert_eq!(execution["kind"], "model");
            assert_eq!(execution["provider"], "codex-personal");
            assert_eq!(execution["provider_kind"], "codex");
            assert_eq!(execution["model"], model);
            assert_eq!(execution["effort"], effort);
            let manifest: toml::Value = toml::from_str(
                &std::fs::read_to_string(
                    p.f.repo
                        .join(format!(".af/task-packages/{worker}/worker.toml")),
                )
                .unwrap(),
            )
            .unwrap();
            let effects: Vec<_> = manifest["signature"]["effects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(
                effects,
                if role == "implementer" {
                    vec!["write-source", "execute-checks"]
                } else {
                    vec!["read-source"]
                }
            );
            if kind == "implement" && role != "implementer" {
                assert_eq!(
                    planned["graph"]["slots"][&slot]["independent_from"],
                    json!(["root.slots.implementer"])
                );
            }
            if role == "verifier" {
                assert_eq!(
                    planned["graph"]["slots"][&slot]["independent_from"],
                    json!(["root.slots.author"])
                );
            }
        }
        let graph = &planned["graph"];
        if kind == "verification" {
            assert!(plan["bindings"].get("root.slots.implementer").is_none());
        } else {
            let consumers: &[&str] = if kind == "implement" {
                &["implement"]
            } else {
                &["author", "verify"]
            };
            assert_eq!(
                graph["nodes"]["root.inputs"]["contract"]["outputs"]["research_report"]["artifact_type"],
                "af/Document@1"
            );
            assert_eq!(
                graph["nodes"]["root.inputs"]["contract"]["outputs"]["research_report"]["optional"],
                true
            );
            // With no predecessor bound yet the compiler omits optional edges. Check
            // their authoring here; the complete loop below proves actual Document delivery.
            let pipeline = document["pipeline"]["name"].as_str().unwrap();
            let manifest: toml::Value = toml::from_str(
                &std::fs::read_to_string(
                    p.f.repo
                        .join(format!(".af/task-packages/{pipeline}/pipeline.toml")),
                )
                .unwrap(),
            )
            .unwrap();
            for consumer in consumers {
                let node = manifest["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|node| node["id"].as_str() == Some(*consumer))
                    .unwrap();
                assert_eq!(
                    node["inputs"]["research_report"]["kind"].as_str(),
                    Some("input")
                );
                assert_eq!(
                    node["inputs"]["research_report"]["port"].as_str(),
                    Some("research_report")
                );
            }
        }
    }
    assert_eq!(stages, ["research", "candidate", "verification", "report"]);
    assert!(
        !marker.exists(),
        "template capture and native planning must invoke no Provider inference"
    );
}

#[test]
fn shipped_performance_loop_delivers_research_and_publishes_verified_improvement_with_recovery() {
    // Reproduce a native Gate's read-only source without changing the sealed checkout.
    // Keep every copied input's bytes and mode to prove only disposable consumer files change.
    let source = tempfile::tempdir().unwrap();
    for directory in [".af", "fixtures/performance/schema/evidence-packet"] {
        let input = if directory == ".af" {
            workspace().join("fixtures/native-catalog-contracts/.af")
        } else {
            workspace().join(directory)
        };
        crate::task_cli::copy_tree(&input, &source.path().join(directory));
    }
    let mut sealed = Vec::new();
    let mut stack = vec![source.path().to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(
                std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else {
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            let mode = permissions.mode() & !0o222;
            permissions.set_mode(mode);
            std::fs::set_permissions(&path, permissions).unwrap();
            assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
            assert_eq!(
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::PermissionDenied,
                "sealed fixture source must refuse writes: {}",
                path.display()
            );
            sealed.push((path.clone(), std::fs::read(path).unwrap(), mode));
        }
    }
    let p = Performance::from_source(source.path());
    let mut bundle = p.bundle();
    let prior = p.f.machine.join("prior-evidence");
    std::fs::create_dir(&prior).unwrap();
    let (_, _, prior_receipt, packet) = native_prior_packet(&prior);
    write(&p.f.machine.join("research-packet.json"), &packet);
    std::fs::write(
        p.home.join("expect-native-findings"),
        "verify useful prior evidence",
    )
    .unwrap();
    let history = &mut bundle["definitions"]["performance"]["steps"][0];
    history["inputs"]["subject"]["value"]["value"] = packet["index_digest"].clone();
    history["inputs"]["revision"]["value"]["value"] = packet["compatibility"].clone();
    valid("scenario-bundle-v11.json", &bundle);
    write(&p.f.bundle, &bundle);
    let pin = p.cli(
        &p.f.state,
        &[
            "scenario",
            "validate",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
        ],
    );
    let waiting = p.cli(
        &p.f.state,
        &[
            "scenario",
            "start",
            "full-loop",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
            "--inputs",
            p.f.inputs.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
            "--execute",
        ],
    );
    if waiting["status"] != "waiting" {
        for target in waiting["fresh_tasks"].as_object().unwrap().values() {
            let detail = p.cli(
                &p.task_state,
                &["task", "show", target["task_id"].as_str().unwrap()],
            );
            eprintln!("native Task failure: {detail:#}");
        }
    }
    assert_eq!(waiting["status"], "waiting", "{waiting:#}");
    assert_eq!(waiting["waiting"]["kind"], "reconcile", "{waiting:#}");
    let calls_before_wait = std::fs::read(p.home.join("requests.jsonl")).unwrap();
    let unpaid_wait = p.cli(&p.f.state, &["scenario", "resume", "full-loop"]);
    assert_eq!(
        std::fs::read(p.home.join("requests.jsonl")).unwrap(),
        calls_before_wait,
        "waiting invokes no model"
    );
    assert_eq!(unpaid_wait["status"], "waiting", "{unpaid_wait}");
    assert_eq!(unpaid_wait["waiting"]["kind"], "condition");
    assert_eq!(unpaid_wait["fresh_tasks"], waiting["fresh_tasks"]);
    let source_intent = p.f.machine.join("native-source-intent");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&source_intent)
        .unwrap();
    let candidate = &waiting["frames"]["root"]["steps"][2]["outputs"]["result"]["value"];
    let request = json!({"schema":"af/scenario-experiment-source-request@1","receipt":candidate,
        "baseline_snapshot_id":null,"candidate_snapshot_id":candidate["result"]["outputs"]["snapshot"]["snapshot_id"],
        "control_sha":p.commit,"trusted_ref":"refs/heads/main"});
    write(&source_intent.join("source-request.json"), &request);
    let (binary, binary_digest) = installed_native_reader(&p.f.machine);
    let source_config = p.f.machine.join("source-config.json");
    write(
        &source_config,
        &json!({"af":binary,"af_digest":binary_digest,"task_repo":p.f.repo,"task_state":p.task_state}),
    );
    let capture = || {
        // Exercise the shipped Python/native bridge, including its distinct request and CAS
        // digest domains. Replacing this bridge with a fake capture would miss that mismatch.
        // Do not cache these inline probes' imports in the shared measurement library:
        // later fixtures copy it as authority and correctly refuse bytecode caches.
        let output = std::process::Command::new("/usr/bin/python3").arg("-IB").arg("-c")
            .arg("import json,pathlib,sys; sys.path.insert(0,sys.argv[1]); from perflab import scenario_source; from perflab.values import canonical_digest; c=json.loads(pathlib.Path(sys.argv[2]).read_text()); p=sys.argv[3]; r=json.loads((pathlib.Path(p)/'source-request.json').read_text()); print(json.dumps(scenario_source.capture(c,p,canonical_digest(r))))")
            .arg(workspace().join("scripts/performance")).arg(&source_config).arg(&source_intent)
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
        valid("scenario-experiment-source-v1.json", &receipt);
        receipt
    };
    let captured = capture();
    assert_eq!(capture(), captured);
    assert_eq!(
        std::fs::read(p.home.join("requests.jsonl")).unwrap(),
        calls_before_wait,
        "native Snapshot capture invokes no model"
    );
    // Fake native completion is the sole service seam. Native source materialization and all
    // four Tasks are real; this test does not run a benchmark or assert a speedup.
    let mut completion: Value = serde_json::from_slice(
        &std::fs::read(
            workspace().join("fixtures/performance/schema/scenario-completion/valid.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut verdict: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/scenarios/performance-positive-completion.json"))
            .unwrap(),
    )
    .unwrap();
    verdict = verdict["verdict"].take();
    verdict["provenance"] = json!("measured");
    verdict["run_id"] = json!("local_synthetic");
    completion["verdict"] = verdict;
    completion["baseline_snapshot_id"] = captured["baseline_snapshot_id"].clone();
    completion["candidate_snapshot_id"] = captured["candidate_snapshot_id"].clone();
    completion["source_request_digest"] = captured["request_digest"].clone();
    completion["request"]["sources"]["baseline_sha"] = captured["baseline_sha"].clone();
    completion["request"]["sources"]["candidate_sha"] = captured["candidate_sha"].clone();
    completion["request"]["sources"]["control_sha"] = json!(p.commit);
    let submitted: Value =
        serde_json::from_slice(&std::fs::read(p.f.machine.join("submission.json")).unwrap())
            .unwrap();
    let experiment: Value =
        serde_json::from_str(submitted["inputs"]["body"]["value"].as_str().unwrap()).unwrap();
    completion["experiment"] = experiment.clone();
    completion["request"]["producer_digest"] = experiment["producer_digest"].clone();
    // The native request records its frozen configuration, while the experiment records
    // the installed configuration. This fixture uses an unchanged frozen configuration.
    completion["request"]["configuration_digest"] = experiment["configuration_digest"].clone();
    write(&p.f.machine.join("completion.json"), &completion);
    let checked = std::process::Command::new("/usr/bin/python3").arg("-IB").arg("-c")
        .arg("import pathlib,sys; sys.path.insert(0,sys.argv[1]); from perflab import contracts; contracts.read_json(pathlib.Path(sys.argv[2]).read_bytes(),'scenario-completion')")
        .arg(workspace().join("scripts/performance")).arg(p.f.machine.join("completion.json")).output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let done = p.cli(&p.f.state, &["scenario", "resume", "full-loop"]);
    if done["status"] != "succeeded" {
        for target in done["fresh_tasks"].as_object().unwrap().values() {
            let detail = p.cli(
                &p.task_state,
                &["task", "show", target["task_id"].as_str().unwrap()],
            );
            eprintln!(
                "Task {}: state {}, failures {}, reports {}",
                target["task_id"], detail["state"], detail["failures"], detail["run_reports"]
            );
        }
    }
    assert_eq!(done["status"], "succeeded", "{done:#}");
    valid("scenario-run-v12.json", &done);
    let steps = done["frames"]["root"]["steps"].as_array().unwrap();
    let research = &steps[1]["outputs"]["result"]["value"];
    let id = research["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let requests = std::fs::read_to_string(p.home.join("requests.jsonl")).unwrap();
    let requests: Vec<Value> = requests
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let candidate_template =
        &bundle["definitions"]["performance"]["steps"][2]["operation"]["template"]["document"];
    assert_owner_performance_instruction_contracts(&p.f.repo, candidate_template);
    let candidate_context = requests
        .iter()
        .find(|r| r["output_schemas"].get("report").is_some())
        .unwrap();
    let inputs = &candidate_context["inputs"];
    let requirements = &inputs["requirements"][0]["payload"];
    assert!(
        requirements["text"]
            .as_str()
            .unwrap()
            .starts_with(candidate_template["goal"].as_str().unwrap())
    );
    assert_eq!(requirements["specification"]["scenario_mode"], "candidate");
    assert!(!requirements["specification"]["scenario_goal"].is_null());
    assert!(!requirements["specification"]["scenario_evidence"].is_null());
    assert_eq!(
        inputs["research_report"][0]["artifact_type"],
        "af/Document@1"
    );
    assert_eq!(inputs["research_report"][0]["artifact_id"], id);
    let cas = Cas::open_existing(p.task_state.join("cas")).unwrap();
    assert_eq!(
        inputs["research_report"][0]["payload"],
        cas.get_artifact(id).unwrap().payload,
        "candidate consumes the exact accepted native research Document"
    );
    let input_schema: Value = serde_json::from_slice(
        &std::fs::read(
            p.f.repo
                .join(".af/task-packages/owner-default/implementer/input.schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    jsonschema::validator_for(&input_schema)
        .unwrap()
        .validate(inputs)
        .unwrap();
    let consumers: Vec<_> = requests
        .iter()
        .filter(|r| r["inputs"].get("research_report").is_some())
        .collect();
    assert_eq!(
        consumers.len(),
        3,
        "candidate, report author and report verifier consume research"
    );
    for r in consumers {
        assert_eq!(r["inputs"]["research_report"][0]["artifact_id"], id);
        assert!(
            r["inputs"]["research_report"][0]["payload"]["text"]
                .as_str()
                .unwrap()
                .contains("DISTINCT-RESEARCH")
        );
    }
    let cas = Cas::open_existing(p.task_state.join("cas")).unwrap();
    let final_id =
        steps[6]["outputs"]["result"]["value"]["result"]["outputs"]["report"]["artifact_ids"][0]
            .as_str()
            .unwrap();
    assert!(
        cas.get_artifact(final_id).unwrap().payload["text"]
            .as_str()
            .unwrap()
            .contains(id)
    );
    assert!(
        cas.get_artifact(final_id).unwrap().payload["text"]
            .as_str()
            .unwrap()
            .contains(prior_receipt["artifact"]["artifact_id"].as_str().unwrap())
    );
    let before = std::fs::read(p.home.join("requests.jsonl")).unwrap();
    assert_eq!(
        p.cli(&p.f.state, &["scenario", "resume", "full-loop"]),
        done
    );
    assert_eq!(
        std::fs::read(p.home.join("requests.jsonl")).unwrap(),
        before
    );
    let roadmap = p.cli(
        &p.f.state,
        &["scenario", "roadmap", "show", "performance-native"],
    );
    let progress = roadmap["progress"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(progress["state"], "completed");
    assert_eq!(progress["reserved_tokens"], 0);
    assert_eq!(progress["task_receipts"].as_array().unwrap().len(), 4);
    assert_eq!(done["fresh_tasks"].as_object().unwrap().len(), 4);
    assert_eq!(
        p.cli(&p.task_state, &["task", "list"])["tasks"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    performance_publication_proof_and_installed_adapter(&p, &done, &binary);
    let mut refused_bundle = bundle.clone();
    let definition = &mut refused_bundle["definitions"]["performance"];
    definition["admission"] = Value::Null;
    definition["steps"].as_array_mut().unwrap().truncate(3);
    definition["steps"][1]["operation"] = json!({"kind":"task","target":{"repo":p.f.repo,"state":p.task_state,"task_id":research["task_id"],"plan_id":research["plan_id"],"revision_id":research["revision_id"],"max_tokens":5000000}});
    definition["steps"][1]["inputs"] = json!({});
    definition["authority"]["task_plans"] = json!([research["plan_id"]]);
    definition["outputs"] = json!({"candidate":"task"});
    definition["result"] =
        json!({"candidate":{"source":"step","step":"candidate","port":"result"}});
    definition["completion"] = json!([literal("boolean", json!(true))]);
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&p.f.policy).unwrap()).unwrap();
    policy["task_plans"] = json!([research["plan_id"]]);
    write(&p.f.policy, &policy);
    for (label, port, kind) in [
        ("missing-research", "absent", "af/Document@1"),
        ("mistyped-research", "report", "af/SourceTree@1"),
    ] {
        refused_bundle["definitions"]["performance"]["steps"][2]["operation"]["bindings"]["research_report"] =
            json!({"port":port,"artifact_type":kind});
        let failed = p.start(label, &refused_bundle);
        assert_eq!(failed["status"], "failed", "{failed}");
        assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
    }
    refused_bundle["definitions"]["performance"]["steps"][2]["operation"]["bindings"]["research_report"] =
        json!({"port":"report","artifact_type":"af/Document@1"});
    // Identical bytes at another commit still lack the exact pinned research provenance.
    crate::task_report::git(
        &p.f.repo,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "different source revision",
        ],
    );
    let out = std::process::Command::new("git")
        .current_dir(&p.f.repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(out.stdout).unwrap();
    let file = p.f.machine.join("candidate-template.json");
    let bindings = p.bindings("implement");
    let pin = p.cli(
        &p.f.state,
        &[
            "scenario",
            "template-pin",
            "--file",
            file.to_str().unwrap(),
            "--repo",
            p.f.repo.to_str().unwrap(),
            "--task-state",
            p.task_state.to_str().unwrap(),
            "--authority-commit",
            commit.trim(),
            "--bindings",
            bindings.to_str().unwrap(),
        ],
    );
    refused_bundle["definitions"]["performance"]["steps"][2]["operation"]["template"] =
        pin["template"].clone();
    refused_bundle["definitions"]["performance"]["authority"]["task_templates"] =
        json!([pin["template_id"]]);
    policy["task_templates"] = json!([pin["template_id"]]);
    write(&p.f.policy, &policy);
    let failed = p.start("wrong-source-research", &refused_bundle);
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(
        failed["failure"]
            .as_str()
            .unwrap()
            .contains("different source"),
        "{failed}"
    );
    assert!(failed["fresh_tasks"].as_object().unwrap().is_empty());
    assert_eq!(
        std::fs::read(p.home.join("requests.jsonl")).unwrap(),
        before
    );
    for (path, bytes, mode) in sealed {
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode(), mode);
    }
}

fn native_prior_packet(root: &Path) -> (PathBuf, PathBuf, Value, Value) {
    // This helper owns its fixture paths. Resolve macOS's /var alias before handing
    // them to the exporter, whose production boundary correctly refuses aliases.
    let canonical_root = root.canonicalize().unwrap();
    let root = canonical_root.as_path();
    let (repo, state) = crate::task_cli::fixture_named(root, "continue");
    let worker = repo.join(".af/task-packages/fixture/bugs/worker.py");
    let original = std::fs::read_to_string(&worker).unwrap();
    let marker = "# A declared base before the pinned catalog";
    assert!(original.contains(marker));
    std::fs::write(&worker, original.replace(marker, "reports.append({'severity':'minor','file':'pagination.py','line':1,'title':'Cache manifest decoding','body':'Decode the exact digest once per source capture.','fix':'Reuse the decoded manifest.','confidence':0.8})\n# A declared base before the pinned catalog")).unwrap();
    crate::task_report::repin_and_commit(&repo);
    let first = value(
        output(
            &repo,
            &state,
            &["task", "start", "--execute", "--file", "first.json"],
        ),
        3,
    );
    assert_eq!(first["state"]["outcome"], "changes_requested");
    let second = cli(&repo, &state, &["task", "run", "second", "--from", "first"]);
    let id = second["continues"]["findings_id"].as_str().unwrap();
    let args = [
        "scenario",
        "evidence-source",
        "second",
        "--artifact-id",
        id,
        "--repo",
        repo.to_str().unwrap(),
        "--task-state",
        state.to_str().unwrap(),
    ];
    let native = cli(&repo, &state, &args);
    valid("scenario-evidence-source-v1.json", &native);
    assert_eq!(
        native["artifact"]["payload"]["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let sources = root.join("sources.json");
    let compat = format!("sha256:{}", "1".repeat(64));
    write(
        &sources,
        &json!([{"task_id":"second","artifact_id":id,"observed_unix_ms":100,"compatibility":compat}]),
    );
    let (binary, binary_digest) = installed_native_reader(root);
    let evidence = root.join("evidence");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&evidence)
        .unwrap();
    let exported = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(workspace().join("scripts/performance/scenario.py"))
        .args([
            "evidence",
            "--sources",
            sources.to_str().unwrap(),
            "--af",
            binary.to_str().unwrap(),
            "--af-digest",
            &binary_digest,
            "--task-repo",
            repo.to_str().unwrap(),
            "--task-state",
            state.to_str().unwrap(),
            "--output-root",
            evidence.to_str().unwrap(),
            "--compatibility",
            &compat,
            "--cutoff",
            "100",
            "--created",
            "100",
            "--max-age-ms",
            "100",
            "--max-items",
            "2",
            "--max-bytes",
            "4096",
        ])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let exported: Value = serde_json::from_slice(&exported.stdout).unwrap();
    let bytes = std::fs::read(exported["packet"].as_str().unwrap()).unwrap();
    assert!(bytes.len() <= 4096);
    let packet: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(packet["items"].as_array().unwrap().len(), 2);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("Nothing says what offset and limit mean"));
    assert!(text.contains("Decode the exact digest once"));
    for item in packet["items"].as_array().unwrap() {
        assert_eq!(item["artifact"]["native_receipt"]["artifact_id"], id);
    }
    let observed = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-c", r#"
import copy, json, os, sys
sys.dont_write_bytecode = True
sys.path.insert(0, sys.argv[1])
from perflab import scenario_adapter, research_evidence
from perflab.values import canonical_digest
root, binary, digest, repo, state, index_path = sys.argv[2:]
os.mkdir(root, 0o700)
spool, evidence = root+'/spool', root+'/evidence'
os.mkdir(spool, 0o700); os.mkdir(evidence, 0o700)
# Only observation is under test: no executor configuration or experiment is needed.
# Retain the real writer, capacity checks, native reader and packet bounds.
a = scenario_adapter.Adapter.__new__(scenario_adapter.Adapter)
a.root = spool
a.native_config = dict(root=root, max_bytes=1<<30)
a.config = dict(root=spool, evidence_root=evidence, max_bytes=1<<30,
    af=binary, af_digest=digest, task_repo=repo, task_state=state,
    destination='native:claw', evidence_max_age_ms=86400000,
    evidence_max_items=32, evidence_max_bytes=24576)
index = json.load(open(index_path))
def observe(records):
    current = research_evidence.rebuild(records)
    a.config['evidence_index'] = research_evidence.retain(evidence, current, 'evidence-index')
    a.config['evidence_index_digest'] = canonical_digest(current)
    text = lambda v: dict(type='text', value=v)
    return a.request(dict(schema='af/scenario-adapter-request@8', method='observe',
        operation='observe', destination='native:claw', intent_id=digest, input_digest=digest,
        inputs=dict(subject=text(canonical_digest(current)), revision=text(records[0]['compatibility']),
                    condition=text('bounded_evidence'))))
records = index['artifacts']
assert observe(records)['observation']['satisfied']
for field, value in [('summary','fabricated finding'), ('source_digest',digest),
                     ('outcome','positive')]:
    altered = copy.deepcopy(records); altered[0][field] = value
    refused = observe(altered)
    assert refused['outcome'] != 'done' and refused['observation'] is None, refused
for field in ['task_id','artifact_id','revision_id']:
    altered = copy.deepcopy(records)
    altered[0]['native_receipt'][field] = 'nonexistent' if field == 'task_id' else digest
    refused = observe(altered)
    assert refused['outcome'] != 'done' and refused['observation'] is None, refused
"#])
        .arg(workspace().join("scripts/performance"))
        .arg(root.join("adapter-observation"))
        .arg(&binary)
        .arg(&binary_digest)
        .arg(&repo)
        .arg(&state)
        .arg(exported["index"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(
        observed.status.success(),
        "{}",
        String::from_utf8_lossy(&observed.stderr)
    );
    (repo, state, native, packet)
}

#[test]
fn native_findings_reach_bounded_research_packet_and_fabricated_artifacts_are_refused() {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let (repo, state, native, packet) = native_prior_packet(root.path());
    let compat = packet["compatibility"].as_str().unwrap();
    // A valid CAS envelope with fabricated finding text is still not a native source receipt.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let mut payload = native["artifact"]["payload"].clone();
    payload["findings"][0]["body"] = json!("fabricated optimization claim");
    let (fabricated, _) = cas
        .put_artifact(
            "af/TaskReviewFindings@1",
            af_contracts::Producer::KernelOperation {
                run_id: "fabricated".into(),
                node_id: None,
                operation_id: "fabricated".into(),
            },
            vec![],
            None,
            payload,
        )
        .unwrap();
    assert!(
        !output(
            &repo,
            &state,
            &[
                "scenario",
                "evidence-source",
                "second",
                "--artifact-id",
                &fabricated,
                "--repo",
                repo.to_str().unwrap(),
                "--task-state",
                state.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    let (adoption, _) = cas
        .put_artifact(
            "af/OptimizationAdoptionObservation@1",
            af_contracts::Producer::KernelOperation {
                run_id: "fabricated".into(),
                node_id: None,
                operation_id: "fabricated".into(),
            },
            vec![],
            None,
            json!({"schema":"af.optimization-adoption-observation/1","causal_claim":false,
                "adoption_receipt_id":compat,"commit_snapshot_id":compat,"commit_tree_id":compat,
                "equivalence":"equivalent","observed_unix_ms":"100","workload_id":compat,
                "model_id":compat,"engine_id":compat,"environment_id":compat}),
        )
        .unwrap();
    assert!(
        !output(
            &repo,
            &state,
            &[
                "scenario",
                "evidence-source",
                "second",
                "--artifact-id",
                &adoption,
                "--repo",
                repo.to_str().unwrap(),
                "--task-state",
                state.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
}

#[test]
fn shipped_no_worthwhile_loop_accepts_its_report_and_dispatches_no_candidate_or_experiment() {
    let p = Performance::new();
    std::fs::write(p.home.join("no-worthwhile"), "bounded evidenced branch").unwrap();
    let bundle = p.bundle_from("performance-no-worthwhile.json");
    let done = p.start("no-worthwhile", &bundle);
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["fresh_tasks"].as_object().unwrap().len(), 1);
    assert!(!p.f.machine.join("submission.json").exists());
    let output = &done["frames"]["root"]["outputs"]["report"]["value"];
    assert_eq!(output["result"]["acceptance"], "satisfied");
    let id = output["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cas = Cas::open_existing(p.task_state.join("cas")).unwrap();
    assert!(
        cas.get_artifact(id).unwrap().payload["text"]
            .as_str()
            .unwrap()
            .contains("NO-WORTHWHILE")
    );
    assert_eq!(
        p.cli(&p.f.state, &["scenario", "resume", "no-worthwhile"]),
        done
    );
}

#[test]
fn rejected_research_report_never_dispatches_candidate_or_native_experiment() {
    let p = Performance::new();
    std::fs::write(p.home.join("refuse-report"), "independent refusal").unwrap();
    let bundle = p.bundle();
    let done = p.start("rejected-research", &bundle);
    assert_eq!(done["status"], "failed", "{done}");
    assert_eq!(done["fresh_tasks"].as_object().unwrap().len(), 1);
    assert!(!p.f.machine.join("submission.json").exists());
    assert_eq!(
        p.cli(&p.task_state, &["task", "list"])["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(done["frames"]["root"]["steps"].as_array().unwrap().len(), 2);
}

#[test]
fn oversized_accepted_research_report_refuses_before_candidate_dispatch() {
    let p = Performance::new();
    std::fs::write(p.home.join("oversized-research"), "bounded native handoff").unwrap();
    let bundle = p.bundle();
    let failed = p.start("oversized-research", &bundle);
    assert_eq!(failed["status"], "failed", "{failed}");
    assert!(
        failed["failure"].as_str().unwrap().contains("32 KiB"),
        "{failed}"
    );
    assert_eq!(failed["fresh_tasks"].as_object().unwrap().len(), 1);
    assert!(!p.f.machine.join("submission.json").exists());
}

/// Reuse the already accepted real four-Task loop; the only simulated evidence is its native
/// lab service response. No additional Provider calls, benchmark, capture or copy of authority.
fn performance_publication_proof_and_installed_adapter(
    p: &Performance,
    done: &Value,
    binary: &Path,
) {
    let steps = done["frames"]["root"]["steps"].as_array().unwrap();
    let receipt = |name: &str| {
        steps.iter().find(|s| s["step"] == name).unwrap()["outputs"]["result"]["value"].clone()
    };
    let candidate = receipt("candidate");
    let cas = Cas::open_existing(p.task_state.join("cas")).unwrap();
    let revision = cas
        .get_artifact(candidate["revision_id"].as_str().unwrap())
        .unwrap();
    let baseline = af_source_git::task::read_snapshot(
        &cas,
        revision.payload["inputs"]["source"]["snapshot_id"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
    .0;
    let request = json!({"schema":"af/performance-pr-proof-request@1", "candidate":candidate,
        "verification":receipt("verification"),"report":receipt("report"),"evidence":receipt("comparison"),
        "source_commit":p.commit,"source_content_digest":baseline.content_digest});
    valid("performance-pr-proof-request-v1.json", &request);
    let file = p.f.machine.join("performance-proof.json");
    write(&file, &request);
    let args = [
        "scenario",
        "performance-proof",
        "--file",
        file.to_str().unwrap(),
        "--repo",
        p.f.repo.to_str().unwrap(),
        "--task-state",
        p.task_state.to_str().unwrap(),
    ];
    let proof = p.cli(&p.f.state, &args);
    valid("performance-pr-proof-v1.json", &proof);
    let body = proof["body"].as_str().unwrap();
    assert_eq!(body.matches("<!-- af-task-report:v1 -->").count(), 1);
    assert_eq!(body.matches("<!-- /af-task-report -->").count(), 1);
    for name in ["candidate", "verification", "report"] {
        assert!(body.contains(receipt(name)["task_id"].as_str().unwrap()));
    }
    assert!(body.contains("improvement") && body.contains("100") && body.contains("80"));
    for fault in [
        "unaccepted",
        "candidate",
        "source",
        "verification",
        "report",
        "base",
        "baseline",
        "non-improving",
        "diagnostic",
        "unsupported",
        "inconclusive",
        "report-evidence",
    ] {
        let mut wrong = request.clone();
        match fault {
            "unaccepted" => wrong["candidate"]["result"]["acceptance"] = json!("unsatisfied"),
            "candidate" => {
                wrong["candidate"]["result"]["outputs"]["snapshot"]["snapshot_id"] =
                    json!(format!("sha256:{}", "0".repeat(64)))
            }
            "source" => {
                wrong["source_content_digest"] = json!(format!("sha256:{}", "0".repeat(64)))
            }
            "verification" => wrong["verification"] = wrong["candidate"].clone(),
            "report" => wrong["report"] = wrong["verification"].clone(),
            "base" => wrong["source_commit"] = json!("0".repeat(40)),
            other => {
                let mut c: Value =
                    serde_json::from_str(wrong["evidence"]["evidence"].as_str().unwrap()).unwrap();
                match other {
                    "baseline" => {
                        c["baseline_snapshot_id"] = json!(format!("sha256:{}", "0".repeat(64)))
                    }
                    "non-improving" => {
                        for case in c["verdict"]["cases"].as_array_mut().unwrap() {
                            case["statistical_classification"] = json!("within_tolerance");
                        }
                    }
                    "diagnostic" => c["experiment"]["work"] = json!("telemetry"),
                    "unsupported" => c["verdict"]["provenance"] = json!("synthetic"),
                    "inconclusive" => c["verdict"]["aggregate"]["result"] = json!("incomplete"),
                    _ => c["result_sha256"] = json!(format!("sha256:{}", "0".repeat(64))),
                }
                wrong["evidence"]["evidence"] = json!(serde_json::to_string(&c).unwrap());
            }
        }
        write(&file, &wrong);
        let out = p.output(&p.f.state, &args);
        assert!(
            !out.status.success(),
            "accepted {fault}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    write(&file, &request);
    let policy: Value = serde_json::from_slice(&std::fs::read(&p.f.policy).unwrap()).unwrap();
    let integration = json!({"request":request,"repo":p.f.repo,"task_state":p.task_state,"af":binary,
        "lab":policy["adapters"]["lab"],"targets":done["fresh_tasks"],"source_commit":p.commit,"source_content_digest":baseline.content_digest});
    let fixture = p.f.machine.join("performance-publication-integration.json");
    write(&fixture, &integration);
    let out = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(workspace().join("scripts/test-scenario-git-workspace.py"))
        .arg("--native-performance-fixture")
        .arg(&fixture)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &p.f.machine)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn implementation_startup_admits_personal_native_pool_without_inference_and_reuses_unpaid_plans() {
    let p = Performance::new();
    let marker = p.home.join("unexpected-startup-inference");
    let trap = format!(
        r#"#!/usr/bin/python3 -B
import json,sys,pathlib
if sys.argv[1:]==['--version']:
 print('codex-cli 0.121.0'); sys.exit(0)
if sys.argv[1:2]==['app-server']:
 for line in sys.stdin:
  r=json.loads(line)
  if r.get('id')==1: print(json.dumps({{'id':1,'result':{{}}}}),flush=True)
  if r.get('id')==2: print(json.dumps({{'id':2,'result':{{'account':{{'type':'chatgpt','email':'scenario@example.invalid'}}}}}}),flush=True)
 sys.exit(0)
pathlib.Path({marker:?}).write_text('unexpected inference')
sys.exit(99)
"#,
        marker = marker.display().to_string()
    );
    for name in ["codex", "claude"] {
        let executable = p.f.machine.join("bin").join(name);
        std::fs::write(&executable, crate::provider_cli::with_help(name, &trap)).unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let original = p.bundle();
    let template = original["definitions"]["performance"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["operation"]["template"]["document"]["kind"] == "implement")
        .unwrap()["operation"]["template"]
        .clone();
    let template: af_scenario::template::Template = serde_json::from_value(template).unwrap();
    let template_id = template.id().unwrap();
    let limits =
        json!({"max_tokens":template.max_tokens,"max_wall_ms":604800000,"max_dispatches":64});
    let bundle: af_scenario::contract::Bundle = serde_json::from_value(json!({"schema":"af/scenario-bundle@11","root":"startup","definitions":{"startup":{
        "schema":"af/scenario@11","inputs":{},"outputs":{"ok":"boolean"},"steps":[step("implement",json!({"kind":"fresh_task","template":template,"bindings":{}}),json!({}),"task")],
        "result":{"ok":literal("boolean",json!(true))},"completion":[literal("boolean",json!(true))],
        "authority":{"task_plans":[],"task_templates":[template_id],"grants":[],"decision_owners":[]},"limits":limits,"admission":null}}})).unwrap();
    let models: std::collections::BTreeMap<_, _> = [
        ("implementer", "gpt-6.2-sol", "medium"), ("bugs", "gpt-6.1-terra", "high"),
        ("correctness", "gpt-6.1-terra", "high"), ("evaluator", "gpt-6.1-terra", "medium")
    ].into_iter().map(|(role, model, effort)| (format!("owner-default/{role}"), json!({"provider":"codex-personal","provider_kind":"codex","model":model,"effort":effort}))).collect();
    let scope = json!({"schema":"af/implementation-startup-scope@2", "owner":"owner", "recipe_id":af_scenario::contract::hash(&bundle).unwrap(),
        "repo":p.f.repo, "task_state":p.task_state, "repository_id":template.repository_id, "source_commit":template.authority_commit,"source_content_digest":template.source_content_digest,
        "repository":"owner/project","destination_repository_id":1,"actor_id":2,"actor_login":"owner","branch_prefix":"implementation/","base":"main","grants":[],
        "templates":{template_id.clone():models},"limits":limits,"max_repairs":0,"expires_unix_ms":8_000_000_000_000_u64});
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&p.f.policy).unwrap()).unwrap();
    policy["implementation_startup"] = scope.clone();
    write(&p.f.policy, &policy);
    write(&p.f.bundle, &serde_json::to_value(&bundle).unwrap());
    let pin = p.cli(
        &p.f.state,
        &[
            "scenario",
            "validate",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
        ],
    );
    for id in ["startup-first", "startup-second"] {
        let run = p.cli(
            &p.f.state,
            &[
                "scenario",
                "start",
                id,
                "--require-startup-scope",
                "--file",
                p.f.bundle.to_str().unwrap(),
                "--authority",
                p.f.policy.to_str().unwrap(),
                "--inputs",
                p.f.inputs.to_str().unwrap(),
                "--confirm-pin",
                pin["revision"].as_str().unwrap(),
            ],
        );
        assert_eq!(run["pin"]["policy"]["implementation_startup"], scope);
        assert_eq!(run["dispatches"], 0);
        assert_eq!(run["fresh_tasks"], json!({}));
    }
    policy["implementation_startup"]["templates"][&template_id]["owner-default/implementer"]["effort"] =
        json!("high");
    write(&p.f.policy, &policy);
    let changed_pin = p.cli(
        &p.f.state,
        &[
            "scenario",
            "validate",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
        ],
    );
    assert_ne!(changed_pin["revision"], pin["revision"]);
    let refused = p.output(
        &p.f.state,
        &[
            "scenario",
            "start",
            "wrong-effective-pool",
            "--require-startup-scope",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
            "--inputs",
            p.f.inputs.to_str().unwrap(),
            "--confirm-pin",
            changed_pin["revision"].as_str().unwrap(),
        ],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("pool differs"));
    let startup_id = format!("startup-{}", template_id.trim_start_matches("sha256:"));
    let planned = p.cli(&p.task_state, &["task", "show", &startup_id]);
    assert_eq!(planned["attempts"], 0);
    assert_eq!(planned["chargeable_tokens"], "0");
    assert!(
        !marker.exists(),
        "startup admission must never invoke inference"
    );
}

#[test]
fn native_scoped_implementation_publishes_and_repairs_one_pr_without_a_decision() {
    native_scoped_implementation(false, false, false, None, "native");
}

#[test]
fn native_scoped_implementation_refuses_an_accepted_narrow_verifier() {
    native_scoped_implementation(true, false, false, None, "native");
}

#[test]
fn native_scoped_default30s_service_publishes_one_pr_with_exact_native_accounting() {
    native_scoped_implementation(false, true, false, None, "native");
}

#[test]
fn native_scoped_default30s_service_recovers_a_killed_driver_reservation() {
    native_scoped_implementation(false, true, true, None, "native");
}

#[test]
fn native_scoped_fresh_guards_refuse_boundaries_in_independent_runs() {
    for fault in ["actor", "destination", "source", "base"] {
        native_scoped_implementation(false, false, false, Some(fault), "native");
    }
}

#[test]
fn native_scoped_default30s_routes_gate_and_review_findings_through_two_fresh_repairs() {
    native_scoped_implementation(false, true, false, None, "gate-review");
}

#[test]
fn native_scoped_default30s_repairs_rejected_whole_goal_verification_before_publication() {
    native_scoped_implementation(false, true, false, None, "verifier");
}

#[test]
fn native_scoped_driver_recovers_transient_preflight_after_process_restart() {
    native_scoped_implementation(false, true, true, None, "transport");
}

#[test]
fn native_scoped_driver_stops_on_native_budget_exhaustion_with_exact_charge() {
    native_scoped_implementation(false, true, false, None, "budget");
}

#[test]
fn native_scoped_driver_retains_startup_scope_refusal_before_any_task_dispatch() {
    native_scoped_implementation(false, true, false, None, "scope-startup");
}

#[test]
fn native_scoped_driver_retains_a_credential_boundary_before_any_task_dispatch() {
    native_scoped_implementation(false, true, false, None, "credentials");
}

fn native_scoped_implementation(
    narrow: bool,
    continuous: bool,
    interrupt: bool,
    fault: Option<&str>,
    native_mode: &str,
) {
    let p = Performance::new();
    std::fs::write(p.home.join("scoped-mode"), native_mode).unwrap();
    let fixture: Value = serde_json::from_slice(include_bytes!(
        "../../../../fixtures/scenarios/performance-candidate.json"
    ))
    .unwrap();
    let steps = fixture["definitions"]["performance"]["steps"]
        .as_array()
        .unwrap();
    let mut pins = Vec::new();
    for kind in ["implement", "verification"] {
        let mut document = steps
            .iter()
            .find(|s| s["operation"]["template"]["document"]["kind"] == kind)
            .unwrap()["operation"]["template"]["document"]
            .clone();
        document["goal"] = json!("Complete the original whole implementation goal");
        document["requirements"] = json!({"acceptance":["Keep the subject and construct the accepted candidate with independent whole-change verification"]});
        if kind == "verification" {
            document["base"] = json!({"commit":p.commit});
        }
        let file = p.f.machine.join(format!("scoped-{kind}.json"));
        write(&file, &document);
        let binding = p.bindings(kind);
        let pin = p.cli(
            &p.f.state,
            &[
                "scenario",
                "template-pin",
                "--file",
                file.to_str().unwrap(),
                "--repo",
                p.f.repo.to_str().unwrap(),
                "--task-state",
                p.task_state.to_str().unwrap(),
                "--authority-commit",
                &p.commit,
                "--bindings",
                binding.to_str().unwrap(),
            ],
        );
        write(&p.f.machine.join(format!("scoped-{kind}-pin.json")), &pin);
        pins.push(pin);
    }
    let body = p.f.machine.join("body.md");
    std::fs::write(&body, "Native scoped implementation fixture").unwrap();
    std::fs::remove_file(&p.f.bundle).unwrap();
    let output = std::process::Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(workspace().join("scripts/scenario-impl.py"))
        .args(["bundle", "--pin"])
        .arg(p.f.machine.join("scoped-implement-pin.json"))
        .arg("--verification-pin")
        .arg(p.f.machine.join("scoped-verification-pin.json"))
        .args([
            "--destination",
            "github:octo/project",
            "--title",
            "Native scoped change",
            "--body-file",
        ])
        .arg(&body)
        .args([
            "--repairs",
            if native_mode == "native" { "1" } else { "2" },
            "--output",
        ])
        .arg(&p.f.bundle)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut authored: Value = serde_json::from_slice(&std::fs::read(&p.f.bundle).unwrap()).unwrap();
    if narrow {
        let mut document = pins[1]["template"]["document"].clone();
        document["goal"] = json!("Verify only the new file, omitting the original whole goal");
        let file = p.f.machine.join("narrow-verifier.json");
        write(&file, &document);
        let binding = p.bindings("verification");
        let pin = p.cli(
            &p.f.state,
            &[
                "scenario",
                "template-pin",
                "--file",
                file.to_str().unwrap(),
                "--repo",
                p.f.repo.to_str().unwrap(),
                "--task-state",
                p.task_state.to_str().unwrap(),
                "--authority-commit",
                &p.commit,
                "--bindings",
                binding.to_str().unwrap(),
            ],
        );
        pins[1] = pin;
        let root = &mut authored["definitions"]["impl"];
        root["authority"]["task_templates"] =
            json!([pins[0]["template_id"], pins[1]["template_id"]]);
        for step in root["steps"].as_array_mut().unwrap() {
            if step["operation"]["kind"] == "fresh_task"
                && step["operation"]["template"]["document"]["kind"] == "verification"
            {
                step["operation"]["template"] = pins[1]["template"].clone();
            }
        }
    }
    let bundle: af_scenario::contract::Bundle = serde_json::from_value(authored).unwrap();
    assert!(bundle.definitions.values().all(|d| {
        d.steps
            .iter()
            .all(|s| !matches!(s.operation, af_scenario::contract::Kind::Decision { .. }))
    }));
    let remote = p.f.machine.join("remote.git");
    for args in [
        vec!["init", "--bare", "-q", remote.to_str().unwrap()],
        vec![
            "-C",
            p.f.repo.to_str().unwrap(),
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{}:refs/heads/main", p.commit),
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let (binary, binary_digest) = installed_native_reader(&p.f.machine);
    let git = std::fs::canonicalize("/usr/bin/git").unwrap();
    let state_dir = p.f.machine.join("publication");
    let worktrees = p.f.machine.join("worktrees");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&worktrees)
        .unwrap();
    let token = p.f.machine.join("token");
    std::fs::write(&token, "controlled-test-token\n").unwrap();
    let ssh = p.f.machine.join("ssh-config");
    std::fs::write(&ssh, "Host github.com\n  BatchMode yes\n").unwrap();
    for file in [&token, &ssh] {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let template = &pins[0]["template"];
    let config = json!({"schema":"af/git-workspace-config@3", "repository":"octo/project", "repository_id":17,
        "actor":{"id":7,"login":"owner"},"token_file":token,"state_dir":state_dir,"repo":p.f.repo,
        "task_store":p.task_state,"worktree_root":worktrees,"base":"main","branch_prefix":"implementation",
        "source_commit":p.commit,"source_content_digest":template["source_content_digest"],"af":binary,"af_digest":binary_digest,
        "git":git,"git_digest":executable_digest(&git),"ssh_config":ssh,"ssh_config_digest":executable_digest(&ssh),
        "author_name":"Owner","author_email":"owner@example.invalid","required_checks":[{"kind":"check","name":"CI","app_id":4}],
        "reviewers":[],"publication_mode":"exclusive_owned_writer","operations":["prepare_candidate_v1","publish_implementation_pr_v1","observe","cleanup_workspace_v1"],
        "implementation_state":p.f.state});
    let config_file = p.f.machine.join("workspace-config.json");
    write(&config_file, &config);
    std::fs::set_permissions(&config_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let service_file = p.f.machine.join("remote-service.json");
    let launcher = p.f.machine.join("scoped-adapter");
    let helper = workspace().join("fixtures/scenarios/impl/native-publication.py");
    let program = format!(
        "#!/usr/bin/python3 -B\nimport sys,runpy\nsys.argv={:?}\nrunpy.run_path({:?},run_name='__main__')\n",
        vec![
            helper.display().to_string(),
            workspace().display().to_string(),
            config_file.display().to_string(),
            remote.display().to_string(),
            service_file.display().to_string()
        ],
        helper.display().to_string()
    );
    std::fs::write(&launcher, &program).unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = &bundle.definitions[&bundle.root];
    let models = |kind: &str| -> Value {
        [ ("bugs", "gpt-6.1-terra", "high"), ("correctness", "gpt-6.1-terra", "high"), ("evaluator", "gpt-6.1-terra", "medium"), ("implementer", "gpt-6.2-sol", "medium") ].into_iter()
        .filter(|(role,_,_)| kind == "implement" || *role != "implementer")
        .map(|(role,model,effort)| (format!("owner-default/{role}"), json!({"provider":"codex-personal","provider_kind":"codex","model":model,"effort":effort}))).collect::<serde_json::Map<_,_>>().into()
    };
    let scope = json!({"schema":"af/implementation-startup-scope@2","owner":"owner","recipe_id":af_scenario::contract::hash(&bundle).unwrap(),
        "repo":p.f.repo,"task_state":p.task_state,"repository_id":template["repository_id"],"source_commit":p.commit,"source_content_digest":template["source_content_digest"],
        "repository":"octo/project","destination_repository_id":17,"actor_id":7,"actor_login":"owner","branch_prefix":"implementation/","base":"main",
        "grants":root.authority.grants,"templates":{pins[0]["template_id"].as_str().unwrap():models("implement"),pins[1]["template_id"].as_str().unwrap():models("verification")},
        "limits":root.limits,"max_repairs":if native_mode == "native" { 1 } else { 2 },"expires_unix_ms":8_000_000_000_000_u64});
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&p.f.policy).unwrap()).unwrap();
    policy["task_templates"] = json!([pins[0]["template_id"], pins[1]["template_id"]]);
    policy["decision_keys"] = json!({});
    policy["implementation_startup"] = scope;
    policy["adapters"] = json!({"workspace":{"executable":launcher,"digest":executable_digest(&launcher),"operations":config["operations"],
        "destinations":["github:octo/project"],"max_wall_ms":30000,"max_output_bytes":65536}});
    write(&p.f.policy, &policy);
    let preflight = json!({"schema":"af/implementation-startup-preflight@2", "scope_id":af_scenario::contract::hash(&policy["implementation_startup"]).unwrap(), "scope":policy["implementation_startup"]});
    let mut child = std::process::Command::new(&launcher)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        &mut child.stdin.take().unwrap(),
        &serde_json::to_vec(&preflight).unwrap(),
    )
    .unwrap();
    let checked = child.wait_with_output().unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    write(
        &service_file,
        &json!({"prs":[], "posts":0, "pushes":0, "lose_post":true,
        "faults":fault.into_iter().collect::<Vec<_>>(), "delayed_ci":continuous, "interrupt_checkpoint":interrupt, "first_checks_pass":native_mode != "native"}),
    );
    write(&p.f.bundle, &serde_json::to_value(&bundle).unwrap());
    let pin = p.cli(
        &p.f.state,
        &[
            "scenario",
            "validate",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
        ],
    );
    let before = p.cli(&p.task_state, &["task", "list"]);
    let refused = p.output(
        &p.f.state,
        &[
            "scenario",
            "start",
            "incompatible",
            "--file",
            p.f.bundle.to_str().unwrap(),
            "--authority",
            p.f.policy.to_str().unwrap(),
            "--inputs",
            p.f.inputs.to_str().unwrap(),
            "--confirm-pin",
            pin["revision"].as_str().unwrap(),
            "--poll-ms",
            "60000",
            "--execute",
        ],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("incompatible with publication step")
    );
    assert_eq!(p.cli(&p.task_state, &["task", "list"]), before);
    assert!(!p.home.join("requests.jsonl").exists());
    assert!(!p.home.join("calls.jsonl").exists());
    let mut run = p.start_recorded(
        "scoped",
        &serde_json::to_value(&bundle).unwrap(),
        !continuous,
    );
    if let Some(fault) = fault {
        for _ in 0..2 {
            run = p.cli(&p.f.state, &["scenario", "resume", "scoped"]);
        }
        let state: Value = serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
        assert_eq!(state["refusals"], json!([fault]), "{state}; {run}");
        assert_eq!(state["posts"], 0);
        assert_eq!(state["pushes"], 0);
        assert!(state.get("bad_guard_success").is_none());
        assert_eq!(run["waiting"]["step"], "root/publish");
        let cancelled = p.cli(
            &p.f.state,
            &[
                "scenario",
                "cancel",
                "scoped",
                "--reason",
                "Independent boundary regression complete",
            ],
        );
        assert_eq!(cancelled["status"], "cancelled");
        return;
    }
    if matches!(native_mode, "budget" | "credentials" | "scope-startup") {
        if matches!(native_mode, "credentials" | "scope-startup") {
            let mut state: Value =
                serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
            if native_mode == "credentials" {
                state["credentials_unavailable"] = json!(true);
            } else {
                state["actor_id"] = json!(99);
            }
            write(&service_file, &state);
        }
        let stopped = p.cli(&p.f.state, &["scenario", "drive", "scoped"]);
        assert_eq!(stopped["status"], "failed", "{}", stopped["failure"]);
        let driver = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
        assert_eq!(driver["blocker"]["kind"], "boundary");
        let state: Value = serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
        assert_eq!(state["pushes"], 0);
        assert_eq!(state["posts"], 0);
        if native_mode == "budget" {
            assert_eq!(stopped["fresh_tasks"].as_object().unwrap().len(), 1);
            let target = stopped["fresh_tasks"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            let task = p.cli(
                &p.task_state,
                &["task", "show", target["task_id"].as_str().unwrap()],
            );
            let charge = task["chargeable_tokens"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let reservation = stopped["task_charges"]
                .as_object()
                .unwrap()
                .values()
                .next()
                .unwrap();
            assert_eq!(reservation["charged"], charge);
            assert!(charge > reservation["maximum"].as_u64().unwrap());
            assert_eq!(reservation["unknown_usage"], false);
        } else {
            assert!(stopped["fresh_tasks"].as_object().unwrap().is_empty());
            assert!(!p.home.join("requests.jsonl").exists());
            assert!(!p.home.join("calls.jsonl").exists());
        }
        let resumed = p.cli(&p.f.state, &["scenario", "resume", "scoped"]);
        assert_eq!(resumed["fresh_tasks"], stopped["fresh_tasks"]);
        assert_eq!(resumed["task_charges"], stopped["task_charges"]);
        return;
    }
    if continuous {
        assert_eq!(run["status"], "ready");
        assert!(run["fresh_tasks"].as_object().unwrap().is_empty());
        run = drive_native_scoped(&p, &service_file, interrupt);
        assert_native_scoped_accounting(&p, &run, &service_file);
        return;
    }
    if narrow {
        let publication = run["frames"]["root"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["step"] == "publish")
            .unwrap();
        assert!(
            publication["diagnostic"]
                .as_str()
                .unwrap()
                .contains("original whole goal and requirements"),
            "{run}"
        );
        assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 2);
        let remote: Value = serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
        assert_eq!(remote["pushes"], 0);
        assert_eq!(remote["posts"], 0);
        return;
    }
    let publication = run["frames"]["root"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["step"] == "publish")
        .unwrap();
    let intent = publication["intents"].as_array().unwrap().last().unwrap()["id"]
        .as_str()
        .unwrap();
    let request_file = state_dir.join(format!(
        "{}.implementation.json",
        intent.trim_start_matches("sha256:")
    ));
    let request: Value = serde_json::from_slice(&std::fs::read(&request_file).unwrap()).unwrap();
    let proof = p.cli(
        &p.f.state,
        &[
            "scenario",
            "implementation-proof",
            "--file",
            request_file.to_str().unwrap(),
        ],
    );
    valid("implementation-pr-proof-v1.json", &proof);
    assert!(
        proof["task_report"]
            .as_str()
            .unwrap()
            .contains("<!-- af-task-report:v1 -->")
    );
    let live = p.cli(
        &p.f.state,
        &[
            "scenario",
            "implementation-proof",
            "--file",
            request_file.to_str().unwrap(),
            "--live-only",
        ],
    );
    valid("implementation-pr-live-v1.json", &live);
    let negative = p.f.machine.join("negative-proof.json");
    for fault in [
        "unaccepted",
        "foreign",
        "stale",
        "affinity",
        "verifier",
        "destination",
        "missing",
        "retired",
    ] {
        let mut wrong = request.clone();
        match fault {
            "unaccepted" => {
                wrong["inputs"]["candidate"]["value"]["result"]["acceptance"] = json!("unsatisfied")
            }
            "foreign" => wrong["inputs"]["candidate"]["value"]["task_id"] = json!("foreign"),
            "stale" => {
                wrong["inputs"]["verification"]["value"]["result_id"] =
                    json!(format!("sha256:{}", "0".repeat(64)))
            }
            "affinity" => {
                wrong["inputs"]["candidate"]["value"]["result"]["outputs"]["snapshot"]["snapshot_id"] =
                    proof["source_snapshot"].clone()
            }
            "verifier" => wrong["inputs"]["verification"] = wrong["inputs"]["candidate"].clone(),
            "destination" => wrong["destination"] = json!("github:foreign/repository"),
            "missing" => {
                wrong["inputs"]
                    .as_object_mut()
                    .unwrap()
                    .remove("verification");
            }
            _ => wrong["schema"] = json!("af/scenario-adapter-request@5"),
        }
        write(&negative, &wrong);
        let refused = p.output(
            &p.f.state,
            &[
                "scenario",
                "implementation-proof",
                "--file",
                negative.to_str().unwrap(),
            ],
        );
        assert!(
            !refused.status.success(),
            "accepted {fault}: {}",
            String::from_utf8_lossy(&refused.stdout)
        );
    }
    let captured: af_scenario::state::Run = serde_json::from_value(run.clone()).unwrap();
    let typed_request: af_scenario::adapter::Request =
        serde_json::from_value(request.clone()).unwrap();
    let world = af_scenario::store::World {
        schema: "af/scenario-store@18".into(),
        writer_version: env!("CARGO_PKG_VERSION").into(),
        runs: std::collections::BTreeMap::from([(captured.id.clone(), captured.clone())]),
        run_capacity: Default::default(),
        drivers: Default::default(),
        ants: Default::default(),
        roadmaps: Default::default(),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    af_scenario::implementation::authorize(&world, &typed_request, now, true).unwrap();
    for fault in [
        "expiry",
        "cancel",
        "fence",
        "inactive",
        "foreign-plan",
        "deadline",
    ] {
        let mut changed = world.clone();
        let changed = changed.runs.get_mut("scoped").unwrap();
        match fault {
            "expiry" => {
                changed
                    .pin
                    .policy
                    .implementation_startup
                    .as_mut()
                    .unwrap()
                    .expires_unix_ms = now
            }
            "cancel" => {
                changed.cancellation = Some(af_scenario::state::Cancellation {
                    requested_unix_ms: now,
                    reason: "owner cancellation".into(),
                    child_requests: Default::default(),
                })
            }
            "inactive" => changed.status = af_scenario::state::Status::Failed,
            "foreign-plan" => changed.fresh_tasks.clear(),
            other => {
                let progress = changed
                    .frames
                    .get_mut("root")
                    .unwrap()
                    .steps
                    .iter_mut()
                    .find(|s| s.step == "publish")
                    .unwrap();
                if other == "deadline" {
                    progress.deadline_unix_ms = now;
                } else {
                    progress.intents.last_mut().unwrap().phase = af_scenario::state::Phase::Done;
                }
            }
        }
        let refused = af_scenario::store::World {
            runs: std::collections::BTreeMap::from([("scoped".into(), changed.clone())]),
            ..world.clone()
        };
        assert!(
            af_scenario::implementation::authorize(&refused, &typed_request, now, true).is_err(),
            "live scope accepted {fault}"
        );
    }
    run = drive_native_scoped(&p, &service_file, false);
    assert_native_scoped_accounting(&p, &run, &service_file);
    let remote_state: Value =
        serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
    assert_eq!(remote_state["posts"], 1, "{remote_state}");
    assert_eq!(remote_state["pushes"], 2, "{remote_state}");
    assert_eq!(remote_state["patches"], 1, "{remote_state}");
    let body = remote_state["prs"][0]["body"].as_str().unwrap();
    assert_eq!(body.matches("<!-- af-task-report:v1 -->").count(), 1);
    for target in run["fresh_tasks"].as_object().unwrap().values() {
        assert!(body.contains(target["task_id"].as_str().unwrap()), "{body}");
    }
    assert_eq!(run["fresh_tasks"].as_object().unwrap().len(), 4);
    assert!(
        run["frames"]["root"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["decision"].is_null())
    );
    let resumed = p.cli(&p.f.state, &["scenario", "resume", "scoped"]);
    assert_eq!(resumed["fresh_tasks"], run["fresh_tasks"]);
    let again: Value = serde_json::from_slice(&std::fs::read(&service_file).unwrap()).unwrap();
    assert_eq!(again["posts"], 1);
    assert_eq!(again["pushes"], 2);
}

fn assert_native_scoped_accounting(p: &Performance, run: &Value, service_file: &Path) {
    let service: Value = serde_json::from_slice(&std::fs::read(service_file).unwrap()).unwrap();
    assert_eq!(service["posts"], 1, "{service}");
    let mode = std::fs::read_to_string(p.home.join("scoped-mode")).unwrap();
    let ci_repair = mode == "native";
    // The transport regression isolates the interrupted publication of one independently
    // verified candidate. The existing native/gate/review modes retain all repair coverage.
    let expected_tasks = if mode == "transport" { 2 } else { 4 };
    assert_eq!(
        service["pushes"],
        if ci_repair { 2 } else { 1 },
        "{service}"
    );
    assert_eq!(
        service["patches"].as_u64().unwrap_or(0),
        u64::from(ci_repair),
        "{service}"
    );
    assert_eq!(service["prs"].as_array().unwrap().len(), 1);
    assert_eq!(service["prs"][0]["merged"], false);
    assert_eq!(
        run["fresh_tasks"].as_object().unwrap().len(),
        expected_tasks
    );
    let outcomes = run["frames"]["root"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["outputs"]["result"]["value"]["outcome"].as_str())
        .collect::<Vec<_>>();
    let expected = match mode.as_str() {
        "transport" => vec!["verified", "verified"],
        "gate-review" => vec!["gate_failed", "changes_requested", "verified", "verified"],
        "verifier" => vec!["verified", "changes_requested", "verified", "verified"],
        _ => vec!["verified", "verified", "verified", "verified"],
    };
    assert_eq!(outcomes, expected);
    for entry in std::fs::read_dir(p.f.machine.join("publication")).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "source")
        {
            let exported: Value =
                serde_json::from_slice(&std::fs::read(path.join("source.json")).unwrap()).unwrap();
            valid("git-candidate-source-v2.json", &exported);
            if !matches!(mode.as_str(), "native" | "transport") {
                assert_ne!(
                    exported["source_snapshot"], exported["task_source_snapshot"],
                    "a native repair exports its whole original baseline, separately from its immediate source"
                );
            }
        }
    }
    let mut total = 0u64;
    let mut model_calls = 0u64;
    for target in run["fresh_tasks"].as_object().unwrap().values() {
        let task = p.cli(
            &p.task_state,
            &["task", "show", target["task_id"].as_str().unwrap()],
        );
        let charge = task["chargeable_tokens"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        total += charge;
        let settled = task["execution_records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["record"])
            .filter(|r| r["kind"] == "settled")
            .collect::<Vec<_>>();
        let mut ids = std::collections::BTreeSet::new();
        let ledger = settled
            .iter()
            .map(|r| {
                assert!(ids.insert(r["attempt_id"].as_str().unwrap()));
                let tokens = r["charged_tokens"]
                    .as_str()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
                assert!(
                    tokens == 0 || tokens == 5,
                    "Unexpected native fixture charge: {r}"
                );
                if tokens == 5 {
                    model_calls += 1;
                }
                tokens
            })
            .sum::<u64>();
        assert_eq!(
            charge, ledger,
            "native cumulative charge must equal distinct settled Attempt receipts"
        );
        assert!(task.get("unknown_usage_attempts").is_none(), "{task}");
    }
    let charges = run["task_charges"].as_object().unwrap();
    assert_eq!(charges.len(), expected_tasks);
    assert_eq!(
        charges
            .values()
            .map(|c| c["charged"].as_u64().unwrap())
            .sum::<u64>(),
        total
    );
    assert!(charges.values().all(|c| c["unknown_usage"] == false));
    assert!(total > 0);
    let requests = std::fs::read_to_string(p.home.join("requests.jsonl")).unwrap();
    assert!(requests.lines().count() as u64 <= model_calls);
    let calls = std::fs::read_to_string(p.home.join("calls.jsonl")).unwrap();
    assert_eq!(calls.lines().count() as u64, model_calls);
    assert_eq!(total, model_calls * 5);
    let status = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
    valid("scenario-driver-v2.json", &status);
    assert_eq!(status["poll_ms"], 30_000);
    assert_eq!(
        status["frontiers"].as_object().unwrap().len(),
        if ci_repair { 2 } else { 1 }
    );
    assert!(status["acknowledged"].as_array().unwrap().len() <= 4);
    assert!(status["resumes"].as_u64().unwrap() < 32);
    for step in run["frames"]["root"]["steps"].as_array().unwrap() {
        assert!(step["decision"].is_null());
        if step["step"].as_str().unwrap().starts_with("publish") {
            assert_eq!(
                step["deadline_unix_ms"].as_u64().unwrap()
                    - step["started_unix_ms"].as_u64().unwrap(),
                120_000
            );
            assert_eq!(step["status"], "succeeded");
        }
    }
}

fn drive_native_scoped(p: &Performance, service_file: &Path, interrupt: bool) -> Value {
    struct Service(std::process::Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let spawn = || {
        Service(
            crate::common::af()
                .current_dir(&p.f.repo)
                .env("HOME", &p.home)
                .env("PATH", &p.path)
                .env("AF_PROVIDERS_FILE", p.home.join("providers.toml"))
                .args(["scenario", "drive", "scoped", "--json", "--state"])
                .arg(&p.f.state)
                .stdout(std::fs::File::create(p.f.machine.join("driver-stdout")).unwrap())
                .stderr(std::fs::File::create(p.f.machine.join("driver-stderr")).unwrap())
                .spawn()
                .unwrap(),
        )
    };
    let mut service = spawn();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    let mut interrupted = false;
    let run = loop {
        if interrupt && !interrupted && service_file.with_extension("barrier").exists() {
            let before = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
            service.0.kill().unwrap();
            service.0.wait().unwrap();
            let retained = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
            assert_eq!(
                retained, before,
                "killing the CLI must not refund its reservation"
            );
            let before_run = p.cli(&p.f.state, &["scenario", "show", "scoped"]);
            let transport =
                std::fs::read_to_string(p.home.join("scoped-mode")).unwrap() == "transport";
            if transport {
                let mut state: Value =
                    serde_json::from_slice(&std::fs::read(service_file).unwrap()).unwrap();
                state["transport_unavailable"] = json!(true);
                write(service_file, &state);
            }
            service = spawn();
            if std::fs::read_to_string(p.home.join("scoped-mode")).unwrap() == "transport" {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                let retained = loop {
                    let status = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
                    if status["preflight_wait"] == "transport" {
                        break status;
                    }
                    assert!(service.0.try_wait().unwrap().is_none());
                    assert!(std::time::Instant::now() < until, "{status}");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                };
                let waiting = p.cli(&p.f.state, &["scenario", "show", "scoped"]);
                assert!(before_run["dispatches"].as_u64().unwrap() > 0);
                for key in ["execution_id", "dispatches", "fresh_tasks", "task_charges"] {
                    assert_eq!(
                        waiting[key], before_run[key],
                        "preflight must not duplicate native work: {key}"
                    );
                }
                assert_eq!(
                    retained["resumes"].as_u64().unwrap(),
                    before["resumes"].as_u64().unwrap() + 1
                );
                service.0.kill().unwrap();
                service.0.wait().unwrap();
                assert_eq!(
                    p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]),
                    retained
                );
                // A standalone restart before the due tick neither dispatches nor refunds the reservation.
                p.cli(&p.f.state, &["scenario", "drive", "scoped", "--once"]);
                assert_eq!(
                    p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]),
                    retained
                );
                let mut state: Value =
                    serde_json::from_slice(&std::fs::read(service_file).unwrap()).unwrap();
                assert_eq!(state["posts"], 0);
                assert_eq!(state["pushes"], 0);
                state["transport_unavailable"] = json!(false);
                write(service_file, &state);
                service = spawn();
            }
            interrupted = true;
        }
        let run = p.cli(&p.f.state, &["scenario", "show", "scoped"]);
        assert_ne!(
            run["status"],
            "failed",
            "{run}; {}",
            std::fs::read_to_string(p.f.machine.join("driver-stderr")).unwrap()
        );
        if run["waiting"]["step"]
            .as_str()
            .is_some_and(|s| s.starts_with("root/prepare"))
        {
            let fixture: Value =
                serde_json::from_slice(&std::fs::read(service_file).unwrap()).unwrap();
            assert!(
                fixture.get("last_error").is_none(),
                "native preparation fixture failed: {}",
                fixture["last_error"]
            );
        }
        if run["status"] == "waiting" && run["waiting"]["step"] == "root/merged" {
            break run;
        }
        assert!(
            service.0.try_wait().unwrap().is_none(),
            "service exited: {}",
            std::fs::read_to_string(p.f.machine.join("driver-stderr")).unwrap()
        );
        assert!(
            std::time::Instant::now() < deadline,
            "driver did not settle: status={}, waiting={}, fixture={}",
            run["status"],
            run["waiting"],
            std::fs::read_to_string(service_file).unwrap_or_default()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(interrupted, interrupt);
    let cancelled = p.cli(
        &p.f.state,
        &[
            "scenario",
            "cancel",
            "scoped",
            "--reason",
            "Human merge remains external",
        ],
    );
    assert_eq!(cancelled["status"], "cancelled");
    let retained = p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]);
    // Cancellation must settle without consuming or refunding a dispatch reservation.
    while service.0.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "cancelled driver did not settle: retained={}, stderr={}",
            p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]),
            std::fs::read_to_string(p.f.machine.join("driver-stderr")).unwrap()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        p.cli(&p.f.state, &["scenario", "driver-status", "scoped"]),
        retained,
        "cancellation shutdown must preserve the retained driver reservation"
    );
    run
}
